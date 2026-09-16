use kithara_warp::{
    AssetFrame, Beat, BeatAlignment, BeatGridId, BeatGridQuery, BeatGridSnapshot, BeatGridState,
    LoadGeneration, MapAxis, MapPoint, MapPosition, MapRegion, Meter, PresentationFrontier,
    RateTarget, SessionBeat, SessionFrame, SyncOperationId, TransportRevision, WarpMapRevision,
};
use num_traits::ToPrimitive;

/// A warp map admitted for one grid member and awaiting the renderer's
/// acknowledgement.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PreparedSync {
    pub(crate) operation: SyncOperationId,
    pub(crate) warp_map: WarpMapRevision,
    pub(crate) activation: SessionFrame,
    pub(crate) activation_beat: SessionBeat,
    pub(crate) source: u64,
    pub(crate) target: BeatGridId,
    pub(crate) disposition: PreparedDisposition,
}

/// Immutable Free handoff input reserved by the group until its worker claims it.
#[derive(Clone, Debug)]
pub(crate) struct FreePreparing {
    pub(crate) operation: SyncOperationId,
    pub(crate) warp_map: WarpMapRevision,
    pub(crate) target: BeatGridId,
    pub(crate) load: LoadGeneration,
    pub(crate) transport: TransportRevision,
    pub(crate) manual_rate: RateTarget,
    pub(crate) owner: BeatGridSnapshot,
}

/// The owner transition completed when a prepared map reaches presentation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PreparedDisposition {
    Lock,
    Free,
}

impl PreparedSync {
    pub(crate) fn frees_deck(self) -> bool {
        self.disposition == PreparedDisposition::Free
    }
}

/// The beat alignment of one grid member onto its owner's grid and the
/// owner-grid frame on which it becomes audible.
///
/// `source` counts output frames, the axis the decoded stream carries and the
/// axis a Free adoption reports, so one meaning survives either disposition.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct MemberAlignment {
    pub(crate) alignment: BeatAlignment,
    pub(crate) activation: SessionFrame,
    pub(crate) activation_beat: SessionBeat,
    pub(crate) source: u64,
}

#[derive(Clone, Copy)]
pub(super) struct AlignmentPolicy {
    pub(super) playback_rate: Option<RateTarget>,
    pub(super) align_downbeat: bool,
    pub(super) require_future_source: bool,
    pub(super) source_cue: Option<Beat>,
}

/// Host seeks retain an exact downbeat and quantize any between-beat request to
/// the following downbeat, matching the deck's published musical grid.
pub(super) fn host_seek_policy(source: kithara_warp::AlignmentSource) -> AlignmentPolicy {
    AlignmentPolicy {
        playback_rate: source.playback_rate(),
        align_downbeat: true,
        require_future_source: source.requires_future_cue(),
        source_cue: None,
    }
}

/// Aligns the member's beat under the frontier's source frame onto the next
/// whole owner beat after the frontier's output frame.
///
/// Returns the region whose geometry is still missing when either grid cannot
/// answer.
pub(super) fn align_member(
    owner: &BeatGridSnapshot,
    member: &BeatGridSnapshot,
    previous: Option<BeatAlignment>,
    frontier: PresentationFrontier,
    preparation_source: u64,
    policy: AlignmentPolicy,
) -> Result<MemberAlignment, MapRegion> {
    let AlignmentPolicy {
        playback_rate,
        align_downbeat,
        require_future_source,
        source_cue,
    } = policy;
    let source = MapPosition::Asset(
        preparation_source
            .to_f64()
            .and_then(|frame| AssetFrame::new(frame).ok())
            .unwrap_or_default(),
    );
    let frontier_output = MapPosition::Session(frontier.output());
    if member.state() != BeatGridState::Complete {
        return Err(MapRegion::point(source));
    }
    if source_cue.is_none()
        && let Some(playback_rate) = playback_rate
        && (frontier.warp_map().is_none() || previous.is_none())
    {
        return seek_audible_member(
            owner,
            member,
            frontier,
            preparation_source,
            playback_rate,
            align_downbeat,
            require_future_source,
        );
    }
    let member_origin = MapPoint::new(member.stamp(), source);
    let BeatGridQuery::Resolved(member_beat) = member.beat_at_or_next(member_origin) else {
        return Err(MapRegion::point(source));
    };
    let member_frontier_beat = *member_beat.value().value();
    let mut member_beat = source_cue.unwrap_or(
        whole_beat(member_frontier_beat, require_future_source)
            .ok_or_else(|| MapRegion::point(source))?,
    );
    let member_meter = (align_downbeat || source_cue.is_some())
        .then(|| member.meter_at(MapPoint::new(member.stamp(), member_beat)))
        .and_then(|query| match query {
            BeatGridQuery::Resolved(meter) => Some(*meter.value()),
            _ => None,
        });
    if source_cue.is_none()
        && let Some(meter) = member_meter
    {
        member_beat =
            next_downbeat(member_beat, meter, false).ok_or_else(|| MapRegion::point(source))?;
    }
    let BeatGridQuery::Resolved(position) =
        member.position_at(MapPoint::new(member.stamp(), member_beat))
    else {
        return Err(MapRegion::point(source));
    };
    let MapPosition::Asset(source_frame) = *position.value().value() else {
        return Err(MapRegion::point(source));
    };
    let source_frame = f64::from(source_frame)
        .round()
        .to_u64()
        .ok_or_else(|| MapRegion::point(source))?;
    let earliest_output = reachable_output(
        owner,
        member,
        previous,
        frontier,
        playback_rate,
        member_beat,
        source_frame,
    )
    .ok_or_else(|| MapRegion::point(frontier_output))?;
    let output = MapPosition::Session(earliest_output);
    let BeatGridQuery::Resolved(owner_beat) = owner.beat_at(MapPoint::new(owner.stamp(), output))
    else {
        return Err(MapRegion::point(output));
    };
    let mut owner_beat =
        whole_beat(*owner_beat.value().value(), false).ok_or_else(|| MapRegion::point(output))?;
    if let Some(member_meter) = member_meter {
        let owner_meter = match owner.meter_at(MapPoint::new(owner.stamp(), owner_beat)) {
            BeatGridQuery::Resolved(owner_meter) => *owner_meter.value(),
            _ => Meter::new(member_meter.beats_per_bar()).map_err(|_| MapRegion::point(output))?,
        };
        owner_beat = if source_cue.is_some() {
            matching_phase(owner_beat, owner_meter, member_beat, member_meter)
        } else {
            next_downbeat(owner_beat, owner_meter, false)
        }
        .ok_or_else(|| MapRegion::point(output))?;
    }
    let target = MapPoint::new(owner.stamp(), owner_beat);
    let BeatGridQuery::Resolved(position) = owner.position_at(target) else {
        return Err(MapRegion::point(output));
    };
    let MapPosition::Session(activation) = *position.value().value() else {
        return Err(MapRegion::point(output));
    };
    Ok(MemberAlignment {
        alignment: BeatAlignment::new(MapPoint::new(member.stamp(), member_beat), target),
        activation,
        activation_beat: SessionBeat::new(f64::from(owner_beat))
            .map_err(|_| MapRegion::point(output))?,
        source: output_source(member, owner, source_frame)
            .ok_or_else(|| MapRegion::point(output))?,
    })
}

/// Acquires phase for an audible, unmapped member by a forward seek.
///
/// The activation is the first owner beat preparation can reach; the cue is
/// the first member beat, in the owner beat's bar phase, at or after the source
/// the live stream presents at that activation. The old stream therefore never
/// reaches the cue before the seek takes effect.
fn seek_audible_member(
    owner: &BeatGridSnapshot,
    member: &BeatGridSnapshot,
    frontier: PresentationFrontier,
    preparation_source: u64,
    playback_rate: RateTarget,
    align_downbeat: bool,
    require_future_source: bool,
) -> Result<MemberAlignment, MapRegion> {
    let frontier_output = MapPosition::Session(frontier.output());
    let earliest = output_at_source(
        frontier,
        Some(playback_rate),
        preparation_source,
        member.axis(),
        owner.axis(),
    )
    .ok_or_else(|| MapRegion::point(frontier_output))?;
    let output = MapPosition::Session(earliest);
    let BeatGridQuery::Resolved(owner_beat) = owner.beat_at(MapPoint::new(owner.stamp(), output))
    else {
        return Err(MapRegion::point(output));
    };
    let mut owner_beat = whole_beat(*owner_beat.value().value(), require_future_source)
        .ok_or_else(|| MapRegion::point(output))?;
    let member_meter = if align_downbeat {
        let preparation = MapPosition::Asset(
            preparation_source
                .to_f64()
                .and_then(|frame| AssetFrame::new(frame).ok())
                .unwrap_or_default(),
        );
        match member.beat_at_or_next(MapPoint::new(member.stamp(), preparation)) {
            BeatGridQuery::Resolved(beat) => {
                match member.meter_at(MapPoint::new(member.stamp(), *beat.value().value())) {
                    BeatGridQuery::Resolved(meter) => Some(*meter.value()),
                    _ => None,
                }
            }
            _ => None,
        }
    } else {
        None
    };
    let owner_meter = match member_meter {
        Some(member_meter) => Some(
            match owner.meter_at(MapPoint::new(owner.stamp(), owner_beat)) {
                BeatGridQuery::Resolved(meter) => *meter.value(),
                _ => Meter::new(member_meter.beats_per_bar())
                    .map_err(|_| MapRegion::point(output))?,
            },
        ),
        None => None,
    };
    if let Some(meter) = owner_meter {
        owner_beat =
            next_downbeat(owner_beat, meter, false).ok_or_else(|| MapRegion::point(output))?;
    }
    let target = MapPoint::new(owner.stamp(), owner_beat);
    let BeatGridQuery::Resolved(position) = owner.position_at(target) else {
        return Err(MapRegion::point(output));
    };
    let MapPosition::Session(activation) = *position.value().value() else {
        return Err(MapRegion::point(output));
    };
    let elapsed = (i64::from(activation) - i64::from(frontier.output()))
        .max(0)
        .to_f64()
        .ok_or_else(|| MapRegion::point(output))?
        * f64::from(playback_rate.speed());
    let live_source = frontier
        .source()
        .to_f64()
        .ok_or_else(|| MapRegion::point(output))?
        + member.axis().native_frame(
            elapsed
                .ceil()
                .to_u64()
                .ok_or_else(|| MapRegion::point(output))?,
            owner.axis().sample_rate(),
        );
    let live =
        MapPosition::Asset(AssetFrame::new(live_source).map_err(|_| MapRegion::point(output))?);
    let BeatGridQuery::Resolved(member_beat) =
        member.beat_at_or_next(MapPoint::new(member.stamp(), live))
    else {
        return Err(MapRegion::point(live));
    };
    let mut member_beat =
        whole_beat(*member_beat.value().value(), false).ok_or_else(|| MapRegion::point(live))?;
    let member_meter = member_meter.and_then(|_| {
        match member.meter_at(MapPoint::new(member.stamp(), member_beat)) {
            BeatGridQuery::Resolved(meter) => Some(*meter.value()),
            _ => None,
        }
    });
    if let (Some(owner_meter), Some(member_meter)) = (owner_meter, member_meter) {
        member_beat = matching_phase(member_beat, member_meter, owner_beat, owner_meter)
            .ok_or_else(|| MapRegion::point(live))?;
    }
    let BeatGridQuery::Resolved(position) =
        member.position_at(MapPoint::new(member.stamp(), member_beat))
    else {
        return Err(MapRegion::point(live));
    };
    let MapPosition::Asset(source_frame) = *position.value().value() else {
        return Err(MapRegion::point(live));
    };
    let source_frame = f64::from(source_frame)
        .round()
        .to_u64()
        .ok_or_else(|| MapRegion::point(live))?;
    Ok(MemberAlignment {
        alignment: BeatAlignment::new(MapPoint::new(member.stamp(), member_beat), target),
        activation,
        activation_beat: SessionBeat::new(f64::from(owner_beat))
            .map_err(|_| MapRegion::point(output))?,
        source: output_source(member, owner, source_frame)
            .ok_or_else(|| MapRegion::point(output))?,
    })
}

/// Preserves the resident member mapping at a decoded-ahead source boundary.
///
/// Unlike reconciliation, this does not quantize a source beat or select a new
/// downbeat: the decoder continues through the exact source frame.
pub(crate) fn handoff_member(
    owner: &BeatGridSnapshot,
    member: &BeatGridSnapshot,
    previous: Option<BeatAlignment>,
    source: kithara_warp::AlignmentSource,
) -> Result<MemberAlignment, MapRegion> {
    let source_frame = source.preparation_source();
    let source_position = MapPosition::Asset(
        source_frame
            .to_f64()
            .and_then(|frame| AssetFrame::new(frame).ok())
            .ok_or_else(|| MapRegion::point(MapPosition::Asset(AssetFrame::default())))?,
    );
    let member_point = MapPoint::new(member.stamp(), source_position);
    let BeatGridQuery::Resolved(member_beat) = member.beat_at(member_point) else {
        return Err(MapRegion::point(source_position));
    };
    let member_beat = *member_beat.value().value();
    let activation = reachable_output(
        owner,
        member,
        previous,
        source.frontier(),
        source.playback_rate(),
        member_beat,
        source_frame,
    )
    .ok_or_else(|| MapRegion::point(MapPosition::Session(source.frontier().output())))?;
    let owner_point = MapPoint::new(owner.stamp(), MapPosition::Session(activation));
    let BeatGridQuery::Resolved(owner_beat) = owner.beat_at(owner_point) else {
        return Err(MapRegion::point(MapPosition::Session(activation)));
    };
    let owner_beat = *owner_beat.value().value();
    Ok(MemberAlignment {
        alignment: previous.unwrap_or_else(|| {
            BeatAlignment::new(
                MapPoint::new(member.stamp(), member_beat),
                MapPoint::new(owner.stamp(), owner_beat),
            )
        }),
        activation,
        activation_beat: SessionBeat::new(f64::from(owner_beat))
            .map_err(|_| MapRegion::point(MapPosition::Session(activation)))?,
        source: output_source(member, owner, source_frame)
            .ok_or_else(|| MapRegion::point(MapPosition::Session(activation)))?,
    })
}

/// Computes a Free lane activation at the renderer's exact current frontier.
/// It does not schedule or correct musical alignment.
pub(crate) fn free_activation_at_frontier(
    owner: &BeatGridSnapshot,
    frontier: PresentationFrontier,
) -> Result<(u64, SessionFrame, SessionBeat), MapRegion> {
    let source = frontier.source();
    let output = frontier.output();
    let owner_point = MapPoint::new(owner.stamp(), MapPosition::Session(output));
    let BeatGridQuery::Resolved(owner_beat) = owner.beat_at(owner_point) else {
        return Err(MapRegion::point(MapPosition::Session(output)));
    };
    let owner_beat = *owner_beat.value().value();
    let beat = SessionBeat::new(f64::from(owner_beat))
        .map_err(|_| MapRegion::point(MapPosition::Session(output)))?;
    Ok((source, output, beat))
}

/// Scales a member-native source frame onto the output axis the decoded
/// stream carries, so a prepared source means the same thing whether it was
/// aligned here or reported by a Free adoption.
fn output_source(member: &BeatGridSnapshot, owner: &BeatGridSnapshot, source: u64) -> Option<u64> {
    Some(
        member
            .axis()
            .output_frame(source.to_f64()?, owner.axis().sample_rate()),
    )
}

fn reachable_output(
    owner: &BeatGridSnapshot,
    member: &BeatGridSnapshot,
    previous: Option<BeatAlignment>,
    frontier: PresentationFrontier,
    playback_rate: Option<RateTarget>,
    member_beat: Beat,
    source: u64,
) -> Option<SessionFrame> {
    if frontier.warp_map().is_some()
        && let Some(previous) = previous
    {
        let beat = f64::from(*previous.target().value()) + f64::from(member_beat)
            - f64::from(*previous.source().value());
        let beat = Beat::new(beat).ok()?;
        let BeatGridQuery::Resolved(position) =
            owner.position_at(MapPoint::new(owner.stamp(), beat))
        else {
            return None;
        };
        let MapPosition::Session(output) = *position.value().value() else {
            return None;
        };
        return Some(output.max(frontier.output()));
    }
    output_at_source(frontier, playback_rate, source, member.axis(), owner.axis())
}

/// Maps a decoded-ahead source frontier onto the live output axis without a
/// seek.
///
/// `source` and the frontier both count frames of the member's own axis, while
/// the answer is an output frame, so the span crosses the resampler once.
pub(super) fn output_at_source(
    frontier: PresentationFrontier,
    playback_rate: Option<RateTarget>,
    source: u64,
    member_axis: MapAxis,
    owner_axis: MapAxis,
) -> Option<SessionFrame> {
    let Some(playback_rate) = playback_rate else {
        return Some(frontier.output());
    };
    let rate = f64::from(playback_rate.speed());
    if !rate.is_finite() || rate <= 0.0 {
        return None;
    }
    let member_frames = source.saturating_sub(frontier.source());
    let source_frames = member_axis
        .output_frame(member_frames.to_f64()?, owner_axis.sample_rate())
        .to_f64()?;
    let output_frames = (source_frames / rate).ceil().to_i64()?;
    Some(SessionFrame::new(
        i64::from(frontier.output()).checked_add(output_frames)?,
    ))
}

fn whole_beat(beat: Beat, strictly_after: bool) -> Option<Beat> {
    let beat = f64::from(beat);
    let mut whole = beat.ceil();
    if strictly_after && whole == beat {
        whole += 1.0;
    }
    Beat::new(whole).ok()
}

fn next_downbeat(beat: Beat, meter: Meter, strictly_after: bool) -> Option<Beat> {
    let ordinal = f64::from(beat).to_i64()?;
    let downbeat = i64::from(meter.downbeat());
    let beats_per_bar = i64::from(meter.beats_per_bar());
    let phase = (ordinal - downbeat).rem_euclid(beats_per_bar);
    let mut distance = (beats_per_bar - phase).rem_euclid(beats_per_bar);
    if strictly_after && distance == 0 {
        distance = beats_per_bar;
    }
    let ordinal = ordinal.checked_add(distance)?;
    Beat::try_from(kithara_warp::BeatOrdinal::new(ordinal)).ok()
}

fn matching_phase(
    owner_beat: Beat,
    owner_meter: Meter,
    member_beat: Beat,
    member_meter: Meter,
) -> Option<Beat> {
    let member_downbeat = Beat::try_from(member_meter.downbeat()).ok()?;
    let owner_downbeat = Beat::try_from(owner_meter.downbeat()).ok()?;
    let member_phase = (f64::from(member_beat) - f64::from(member_downbeat))
        .rem_euclid(f64::from(member_meter.beats_per_bar()));
    let owner_phase = (f64::from(owner_beat) - f64::from(owner_downbeat))
        .rem_euclid(f64::from(owner_meter.beats_per_bar()));
    let distance = (member_phase - owner_phase).rem_euclid(f64::from(owner_meter.beats_per_bar()));
    Beat::new(f64::from(owner_beat) + distance).ok()
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;
    use kithara_warp::{Beat, BeatOrdinal, Meter};

    use super::matching_phase;

    #[kithara::test]
    fn pickup_track_start_keeps_its_weak_beat_phase() {
        let source_meter = Meter::new(4)
            .expect("four beats per bar")
            .with_downbeat(BeatOrdinal::new(1));
        let host_meter = Meter::new(4).expect("four beats per bar");
        let source = Beat::new(0.0).expect("source beat zero");
        let host_frontier = Beat::new(1.0).expect("first eligible host beat");

        let target = matching_phase(host_frontier, host_meter, source, source_meter)
            .expect("pickup phase resolves");

        assert_eq!(f64::from(target), 3.0);
    }

    #[kithara::test]
    fn pickup_track_start_preserves_fractional_beat_phase() {
        let source_meter = Meter::new(4)
            .expect("four beats per bar")
            .with_downbeat(BeatOrdinal::new(1));
        let host_meter = Meter::new(4).expect("four beats per bar");
        let source = Beat::new(0.5).expect("fractional source beat");
        let host_frontier = Beat::new(1.0).expect("first eligible host beat");

        let target = matching_phase(host_frontier, host_meter, source, source_meter)
            .expect("pickup phase resolves");

        assert_eq!(f64::from(target), 3.5);
    }
}

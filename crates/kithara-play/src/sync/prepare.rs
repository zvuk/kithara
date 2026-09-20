pub(crate) use kithara_sync::MemberAlignment;
use kithara_warp::{
    AssetFrame, Beat, BeatAlignment, BeatGridId, BeatGridQuery, BeatGridSnapshot, BeatGridState,
    MapAxis, MapPoint, MapPosition, MapRegion, Meter, PresentationFrontier, RateTarget,
    SessionBeat, SessionFrame, SyncOperationId, WarpMapRevision,
};
use num_traits::ToPrimitive;

/// A warp map admitted for one grid member and awaiting the renderer's
/// acknowledgement.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PreparedSync {
    pub(crate) operation: SyncOperationId,
    pub(crate) warp_map: WarpMapRevision,
    pub(crate) activation: SessionFrame,
    pub(crate) activation_beat: SessionBeat,
    pub(crate) source: u64,
    pub(crate) target: BeatGridId,
    pub(crate) disposition: PreparedDisposition,
    /// The member's grid as it sounds on the owner's, frozen with this map.
    ///
    /// The map and the projection are prepared together from the same owner
    /// grid, so a renderer cannot hear a map aligned against one grid while
    /// measuring its spans against another.
    pub(crate) projection: BeatGridSnapshot,
}

/// Every warp map this group has prepared, one per member it targets.
///
/// A group prepares an entry for each of its members, not only for the one it
/// hears: a waiting member holds its own prepared map until its activation
/// frame arrives.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct PreparedSyncs(Vec<PreparedSync>);

impl PreparedSyncs {
    /// Replaces whatever this member had prepared.
    pub(crate) fn insert(&mut self, prepared: PreparedSync) {
        self.remove(prepared.target);
        self.0.push(prepared);
    }

    /// The map prepared for one member.
    pub(crate) fn get(&self, target: BeatGridId) -> Option<PreparedSync> {
        self.0
            .iter()
            .find(|prepared| prepared.target == target)
            .cloned()
    }

    /// The map prepared by one operation, which a renderer acknowledges by.
    pub(crate) fn by_operation(&self, operation: SyncOperationId) -> Option<PreparedSync> {
        self.0
            .iter()
            .find(|prepared| prepared.operation == operation)
            .cloned()
    }

    /// The most recently prepared map.
    ///
    /// The group reports one status and names one expected operation for the
    /// whole group, so both read the newest preparation rather than a member.
    pub(crate) fn latest(&self) -> Option<PreparedSync> {
        self.0
            .iter()
            .max_by_key(|prepared| prepared.operation)
            .cloned()
    }

    /// Drops what one member had prepared.
    pub(crate) fn remove(&mut self, target: BeatGridId) {
        self.0.retain(|prepared| prepared.target != target);
    }

    delegate::delegate! {
        to self.0 {
            /// Drops every prepared map, as an axis change or a state change does.
            pub(crate) fn clear(&mut self);
            /// Whether this group holds no prepared map at all.
            pub(crate) fn is_empty(&self) -> bool;
        }
    }
}

/// Immutable Free handoff input reserved by the group until its worker claims it.
#[derive(Clone, Debug)]
pub(crate) struct FreePreparing {
    pub(crate) stamp: kithara_sync::SyncExecutionStamp,
    pub(crate) alignment: MemberAlignment,
    pub(crate) manual_rate: RateTarget,
}

/// The owner transition completed when a prepared map reaches presentation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PreparedDisposition {
    Lock,
    Free,
}

impl PreparedSync {
    pub(crate) fn frees_deck(&self) -> bool {
        self.disposition == PreparedDisposition::Free
    }
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

/// The owner-axis window a waiting member may enter through.
///
/// `earliest` is the first frame the deck can make the member audible on, and
/// `deadline` the last frame the entry still serves, such as the frame the
/// outgoing track starts its fade on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct EntryWindow {
    pub(crate) earliest: SessionFrame,
    pub(crate) deadline: SessionFrame,
}

/// Places a waiting member's first downbeat on a downbeat of the owner grid.
///
/// The entry takes the last owner downbeat the window closes on, so a member
/// enters as late as the deadline allows; a window too short to hold a
/// downbeat takes the first downbeat after it, because entering off the bar
/// would break the phase the group exists to keep.
pub(super) fn enter_member(
    owner: &BeatGridSnapshot,
    member: &BeatGridSnapshot,
    window: EntryWindow,
    source_cue: Option<Beat>,
) -> Result<MemberAlignment, MapRegion> {
    let origin = MapPosition::Asset(AssetFrame::default());
    if member.state() != BeatGridState::Complete {
        return Err(MapRegion::point(origin));
    }
    let BeatGridQuery::Resolved(first) =
        member.beat_at_or_next(MapPoint::new(member.stamp(), origin))
    else {
        return Err(MapRegion::point(origin));
    };
    let first =
        whole_beat(*first.value().value(), false).ok_or_else(|| MapRegion::point(origin))?;
    let member_meter = match member.meter_at(MapPoint::new(member.stamp(), first)) {
        BeatGridQuery::Resolved(meter) => Some(*meter.value()),
        _ => None,
    };
    let member_beat = match (source_cue, member_meter) {
        (Some(cue), _) => cue,
        (None, Some(meter)) => {
            next_downbeat(first, meter, false).ok_or_else(|| MapRegion::point(origin))?
        }
        (None, None) => first,
    };
    let BeatGridQuery::Resolved(position) =
        member.position_at(MapPoint::new(member.stamp(), member_beat))
    else {
        return Err(MapRegion::point(origin));
    };
    let MapPosition::Asset(source_frame) = *position.value().value() else {
        return Err(MapRegion::point(origin));
    };
    let source_frame = f64::from(source_frame)
        .round()
        .to_u64()
        .ok_or_else(|| MapRegion::point(origin))?;
    let phase = source_cue
        .and(member_meter)
        .map(|member_meter| (member_beat, member_meter));
    let owner_beat = entry_beat(owner, window, member_meter, phase)?;
    let target = MapPoint::new(owner.stamp(), owner_beat);
    let BeatGridQuery::Resolved(position) = owner.position_at(target) else {
        return Err(MapRegion::point(MapPosition::Session(window.deadline)));
    };
    let MapPosition::Session(activation) = *position.value().value() else {
        return Err(MapRegion::point(MapPosition::Session(window.deadline)));
    };
    Ok(MemberAlignment {
        alignment: BeatAlignment::new(MapPoint::new(member.stamp(), member_beat), target),
        activation,
        activation_beat: SessionBeat::new(f64::from(owner_beat))
            .map_err(|_| MapRegion::point(MapPosition::Session(activation)))?,
        source: output_source(member, owner, source_frame)
            .ok_or_else(|| MapRegion::point(MapPosition::Session(activation)))?,
    })
}

/// The owner downbeat a waiting member enters on.
///
/// A session grid publishes beats without a meter, so the bar the entry lands
/// on is the member's bar carried onto the owner's beats. A member without a
/// meter of its own enters on any whole beat, its every beat being a downbeat.
fn entry_beat(
    owner: &BeatGridSnapshot,
    window: EntryWindow,
    member_meter: Option<Meter>,
    phase: Option<(Beat, Meter)>,
) -> Result<Beat, MapRegion> {
    let earliest = MapPosition::Session(window.earliest);
    let BeatGridQuery::Resolved(earliest_beat) =
        owner.beat_at(MapPoint::new(owner.stamp(), earliest))
    else {
        return Err(MapRegion::point(earliest));
    };
    let earliest_beat = whole_beat(*earliest_beat.value().value(), false)
        .ok_or_else(|| MapRegion::point(earliest))?;
    let meter = match owner.meter_at(MapPoint::new(owner.stamp(), earliest_beat)) {
        BeatGridQuery::Resolved(meter) => Some(*meter.value()),
        _ => member_meter,
    };
    let opening = match meter {
        Some(meter) => phase_at_or_after(earliest_beat, meter, phase)
            .ok_or_else(|| MapRegion::point(earliest))?,
        None => earliest_beat,
    };
    let deadline = MapPosition::Session(window.deadline);
    let BeatGridQuery::Resolved(deadline_beat) =
        owner.beat_at(MapPoint::new(owner.stamp(), deadline))
    else {
        return Err(MapRegion::point(deadline));
    };
    let deadline_beat = f64::from(*deadline_beat.value().value()).floor();
    let deadline_beat = Beat::new(deadline_beat).map_err(|_| MapRegion::point(deadline))?;
    let latest = match meter {
        Some(meter) => phase_at_or_before(deadline_beat, meter, phase)
            .ok_or_else(|| MapRegion::point(deadline))?,
        None => deadline_beat,
    };
    Ok(if f64::from(latest) < f64::from(opening) {
        opening
    } else {
        latest
    })
}

/// The first owner beat at or after `beat` carrying the entry phase.
///
/// An entry without a member cue lands on a downbeat; an entry that carries one
/// keeps the cue's own bar phase, as an audible alignment does.
fn phase_at_or_after(beat: Beat, meter: Meter, phase: Option<(Beat, Meter)>) -> Option<Beat> {
    match phase {
        Some((member_beat, member_meter)) => matching_phase(beat, meter, member_beat, member_meter),
        None => next_downbeat(beat, meter, false),
    }
}

/// The last owner beat at or before `beat` carrying the entry phase.
fn phase_at_or_before(beat: Beat, meter: Meter, phase: Option<(Beat, Meter)>) -> Option<Beat> {
    let forward = phase_at_or_after(beat, meter, phase)?;
    if f64::from(forward) <= f64::from(beat) {
        return Some(forward);
    }
    Beat::new(f64::from(forward) - f64::from(meter.beats_per_bar())).ok()
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

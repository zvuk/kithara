use std::num::{NonZeroU16, NonZeroU128};

use kithara::{
    beat::BeatGridModel,
    platform::time::Duration,
    signal::{
        AudioChunkInfo, AudioSpec, ExponentialApproach, SegmentId, SessionEpoch, SessionFrame,
        SourceSpan,
    },
    sync::{SessionAnchor, SourceGrid, SpeedCurve, corrected_speed_curve, speed_curve},
    warp::{
        AssetAxis, AssetExtent, AssetFrame, Beat, BeatGridId, BeatGridQuery, BeatGridRevision,
        BeatGridSnapshot, MapPoint, MapPosition,
    },
};
use kithara_command::When;
use kithara_integration_tests::offline::linked::{
    LinkCue, LinkObservation, LinkOwnerInput, LinkOwnerInputKind, LinkPlacement, LinkReceiptOrigin,
    LinkSpeed, LinkSyncRequest,
};
use kithara_render::{
    LaneFrame,
    bridge::{SlotMark, SlotSnapshot, SlotState},
};
use num_traits::ToPrimitive;

use super::{cases::*, harness::*, imports::*, providers::*};

#[derive(Debug)]
pub(super) struct GridBinding {
    mark: SlotMark,
    media_origin: Duration,
    source_beat: f64,
    host_beat: f64,
    expected_frame: SessionFrame,
    expected_speed: f32,
    actual_speed: f32,
}

impl GridBinding {
    pub(super) fn failures(&self) -> Vec<String> {
        let mut failures = Vec::new();
        if self.expected_frame != self.mark.session {
            failures.push(format!(
                "source grid beat {:.12} binds to Host beat {:.12} at {:?}, consumed at {:?}",
                self.source_beat, self.host_beat, self.expected_frame, self.mark.session
            ));
        }
        if self.expected_speed != self.actual_speed {
            failures.push(format!(
                "source/Host grid ratio requires speed {}, consumed track reports {}",
                self.expected_speed, self.actual_speed
            ));
        }
        failures
    }

    pub(super) fn evidence(&self) -> serde_json::Value {
        serde_json::json!({
            "source_beat": self.source_beat,
            "host_beat": self.host_beat,
            "expected_frame": i64::from(self.expected_frame),
            "consumed_session_frame": i64::from(self.mark.session),
            "consumed_lane": format!("{:?}", self.mark.lane),
            "consumed_raw_ns": self.mark.position.as_nanos().to_string(),
            "media_origin_ns": self.media_origin.as_nanos().to_string(),
            "expected_speed": self.expected_speed,
            "actual_speed": self.actual_speed,
        })
    }
}

fn consumed_media_seconds(
    span: SourceSpan,
    output_frame: u64,
    media_origin: Duration,
) -> Option<f64> {
    let (numerator, denominator) = span.source_ratio_at(output_frame)?;
    let denominator = denominator
        .get()
        .checked_mul(u128::from(span.sample_rate().get()))?;
    let numerator = numerator
        .checked_mul(1_000_000_000)?
        .checked_sub(media_origin.as_nanos().checked_mul(denominator)?)?;
    let denominator = denominator.checked_mul(1_000_000_000)?;
    let mut divisor = numerator;
    let mut remainder = denominator;
    while remainder != 0 {
        (divisor, remainder) = (remainder, divisor % remainder);
    }
    Some((numerator / divisor).to_f64()? / (denominator / divisor).to_f64()?)
}

pub(super) fn check(
    model: &BeatGridModel,
    source_rate: NonZeroU32,
    host_grid: &BeatGridSnapshot,
    consumed: SlotSnapshot,
    actual_speed: f32,
    host_beats_per_bar: u16,
    reference: Option<SelectedBinding>,
) -> Result<GridBinding, String> {
    if consumed.state != SlotState::Playing {
        return Err(format!(
            "binding has no consumed playing slot: {:?}",
            consumed.state
        ));
    }
    let mark = consumed
        .mark
        .ok_or_else(|| "binding has no consumed PCM mark".to_owned())?;
    let source = consumed
        .source
        .filter(|info| info.segment == mark.lane.segment)
        .ok_or_else(|| "binding has no current consumed source geometry".to_owned())?;
    let media_origin = source
        .media_origin
        .ok_or_else(|| "binding has no known media origin".to_owned())?;
    let span = source
        .source_span
        .ok_or_else(|| "binding has no exact consumed SourceSpan".to_owned())?;
    let offset = mark
        .lane
        .frame
        .checked_sub(source.lane_frame)
        .ok_or_else(|| "consumed mark precedes its source geometry".to_owned())?;
    if span.position_at(offset) != Some(mark.position) {
        return Err("consumed mark disagrees with its exact source geometry".to_owned());
    }
    let media = consumed_media_seconds(span, offset, media_origin)
        .ok_or_else(|| "consumed source coordinate cannot translate to media time".to_owned())?;
    let source_grid = BeatGridSnapshot::model(
        BeatGridId::allocate().map_err(|error| error.to_string())?,
        BeatGridRevision::first(),
        model,
        AssetAxis::new(source_rate, AssetExtent::Unknown),
    )
    .map_err(|error| error.to_string())?;
    let source_frame =
        AssetFrame::new(media * f64::from(source_rate.get())).map_err(|error| error.to_string())?;
    let source_position = MapPoint::new(source_grid.stamp(), MapPosition::Asset(source_frame));
    let source_query = source_grid.beat_at(source_position);
    let BeatGridQuery::Resolved(source_beat) = source_query else {
        return Err(format!(
            "consumed source position has no resolved supplied-grid beat: {source_query:?}; \
             mark={mark:?}; source={source:?}; offset={offset}; \
             exact_source_ratio={:?}; media_seconds={media:.17}; \
             source_frame={source_frame:?}; source_rate={source_rate}; \
             grid_axis={:?}; grid_state={:?}; model_duration={:?}; \
             model_bpm={}; first_beat={:?}; last_beat={:?}",
            span.source_ratio_at(offset),
            source_grid.axis(),
            source_grid.state(),
            model.as_raw().duration,
            model.as_raw().bpm,
            model.as_raw().beats.first(),
            model.as_raw().beats.last(),
        ));
    };
    let source_beat = f64::from(*source_beat.value().value());
    let host_position = MapPoint::new(host_grid.stamp(), MapPosition::Session(mark.session));
    let BeatGridQuery::Resolved(observed_host_beat) = host_grid.beat_at(host_position) else {
        return Err("consumed session frame has no published Host beat".to_owned());
    };
    let observed_host_beat = f64::from(*observed_host_beat.value().value());
    let (origin, source_stride, host_stride) = phase_axes(model, host_beats_per_bar)?;
    let phase = (source_beat - origin).rem_euclid(source_stride);
    let host_beat = ((observed_host_beat - phase) / host_stride).round() * host_stride + phase;
    let host_target = MapPoint::new(
        host_grid.stamp(),
        Beat::new(host_beat).map_err(|error| error.to_string())?,
    );
    let BeatGridQuery::Resolved(expected_position) = host_grid.position_at(host_target) else {
        return Err("source-grid phase has no Host target frame".to_owned());
    };
    let MapPosition::Session(expected_frame) = *expected_position.value().value() else {
        return Err("published Host grid has a different coordinate axis".to_owned());
    };
    let BeatGridQuery::Resolved(source_tempo) = source_grid.tempo_at(source_position) else {
        return Err("consumed source grid has no local tempo".to_owned());
    };
    let BeatGridQuery::Resolved(host_tempo) = host_grid.tempo_at(host_position) else {
        return Err("consumed Host grid has no local tempo".to_owned());
    };
    let mut expected_speed: f32 =
        (f64::from(*host_tempo.value()) / f64::from(*source_tempo.value())).as_();
    if let Some(reference) = reference {
        expected_speed = binding_speed(&source_grid, host_grid, mark.session, reference)?;
    }
    Ok(GridBinding {
        mark,
        media_origin,
        source_beat,
        host_beat,
        expected_frame,
        expected_speed,
        actual_speed,
    })
}

fn binding_speed(
    source: &BeatGridSnapshot,
    host: &BeatGridSnapshot,
    at: SessionFrame,
    reference: SelectedBinding,
) -> Result<f32, String> {
    let from = reference.source_beat
        + (grid_beat_at(host, MapPosition::Session(at))? - reference.host_beat);
    let next = frame_after(at, 1)?;
    let to = reference.source_beat
        + (grid_beat_at(host, MapPosition::Session(next))? - reference.host_beat);
    let rate = source.axis().sample_rate();
    let start = source_media_for_beat(source, from, rate)?;
    if ordinary_span_is_local(source, from, to, rate, false)? {
        return Ok((grid_tempo(host, MapPosition::Session(at))?
            / grid_tempo(source, media_seconds_position(start, rate)?)?)
        .as_());
    }
    let end = source_media_for_beat(source, to, rate)?;
    Ok(((end - start) * f64::from(host.axis().sample_rate().get())).as_())
}

struct AppliedCurve<'a> {
    command: &'a LinkSpeed,
    at: SessionFrame,
}

struct SourceGeometry<'a> {
    model: &'a BeatGridModel,
    source: BeatGridSnapshot,
    maximum_slope: f64,
}

impl<'a> SourceGeometry<'a> {
    fn new(
        model: &'a BeatGridModel,
        rate: NonZeroU32,
        duration: Option<Duration>,
    ) -> Result<Self, String> {
        let source = source_with_duration(model, rate, duration)?;
        let maximum_slope = maximum_source_slope(model, &source, rate)?;
        Ok(Self {
            model,
            source,
            maximum_slope,
        })
    }
}

struct PrefixPlan {
    latest: bool,
    initial_media: Duration,
    offsets: Vec<(u64, f64)>,
    terminal: u64,
    parts: Vec<(u64, ExponentialApproach)>,
    deadline: Option<u64>,
    limit: u64,
    boundaries: Vec<u64>,
    previous_offset: f64,
}

type PrefixProof = (
    u64,
    Option<u64>,
    Vec<String>,
    Vec<serde_json::Value>,
    SelectedBinding,
);

type LawProof = (f64, Vec<String>, serde_json::Value, Option<SelectedBinding>);

fn declared_binding_at(
    source: &BeatGridSnapshot,
    host: &TempoTrajectory,
    beat: f64,
    frame: SessionFrame,
) -> Result<SelectedBinding, String> {
    Ok(SelectedBinding {
        media: Duration::try_from_secs_f64(source_media_for_beat(
            source,
            beat,
            source.axis().sample_rate(),
        )?)
        .map_err(|error| error.to_string())?,
        source_beat: beat,
        host_beat: f64::from(host.beat_at(frame)),
        frame,
    })
}

fn applied_curves(
    observation: &LinkObservation,
    item: kithara::events::TrackId,
    mark: SlotMark,
    rate: NonZeroU32,
) -> Result<Vec<AppliedCurve<'_>>, String> {
    let mut curves = Vec::new();
    for command in observation
        .speeds
        .iter()
        .filter(|command| command.item == item)
    {
        if !matches!(
            command.before.status,
            kithara::play::TrackStatus::Playing { .. }
        ) {
            continue;
        }
        let Some(before) = command.before.mark else {
            return Err(format!(
                "speed {:?} has no actual pre-command mark",
                command.seq
            ));
        };
        if before.lane.segment != mark.lane.segment {
            continue;
        }
        let slot = command
            .consumed
            .ok_or_else(|| format!("speed {:?} has no consumed source anchor", command.seq))?;
        let current = slot
            .mark
            .ok_or_else(|| format!("speed {:?} has no consumed source mark", command.seq))?;
        if command.sample_rate != Some(rate)
            || slot
                .source
                .filter(|info| info.segment == current.lane.segment)
                .and_then(|info| info.media_origin)
                .is_none()
            || before.lane_at(current.session) != Some(current.lane)
        {
            return Err(format!(
                "speed {:?} lost its consumed source/Host axis",
                command.seq
            ));
        }
        let receipts: Vec<_> = observation.placement_receipts.iter().filter_map(|(owner, origin, settled)| {
            (*owner == item && *origin == LinkReceiptOrigin::Lane && matches!(settled,
                kithara::play::Settled::Applied { seq, .. } | kithara::play::Settled::Rejected { seq, .. } if *seq == command.seq)).then_some(settled)
        }).collect();
        match receipts.as_slice() {
            [kithara::play::Settled::Applied { at, .. }] => {
                if matches!(command.at, When::At(requested) if requested != *at) {
                    return Err(format!(
                        "speed {:?} requested {:?}, actually applied at {:?}",
                        command.seq, command.at, at
                    ));
                }
                curves.push(AppliedCurve { command, at: *at });
            }
            [kithara::play::Settled::Rejected { .. }] => {}
            [] if matches!(command.at, When::At(at) if at >= mark.session) => {}
            [] => {
                return Err(format!(
                    "speed {:?} has no actual Lane outcome",
                    command.seq
                ));
            }
            _ => {
                return Err(format!(
                    "speed {:?} has {} Lane outcomes",
                    command.seq,
                    receipts.len()
                ));
            }
        }
    }
    curves.sort_by_key(|curve| (curve.at, curve.command.seq));
    Ok(curves)
}

fn owned_cue(
    observation: &LinkObservation,
    item: kithara::events::TrackId,
    segment: SegmentId,
) -> Result<&LinkCue, String> {
    observation
        .cue_curves
        .iter()
        .rev()
        .find(|cue| {
            cue.item == item
                && observation.ready_cues.contains(&(item, cue.seq))
                && cue
                    .consumed
                    .and_then(|slot| slot.mark)
                    .is_some_and(|first| first.lane.segment == segment)
        })
        .ok_or_else(|| "On has neither an Applied law nor an owned consumed cue Segment".to_owned())
}

fn configured_speed_failures(
    observation: &LinkObservation,
    item: kithara::events::TrackId,
    segment: SegmentId,
    curves: &[AppliedCurve<'_>],
    configured_speed: f32,
) -> Result<Vec<String>, String> {
    let (seq, curve) = if let Some(applied) = curves.last() {
        (applied.command.seq, &applied.command.curve)
    } else {
        let cue = owned_cue(observation, item, segment)?;
        (cue.seq, &cue.curve)
    };
    let target = analytic_parts(curve)?
        .last()
        .ok_or_else(|| "confirmed curve is empty".to_owned())?
        .1
        .target();
    Ok(if target.to_f32() == Some(configured_speed) {
        Vec::new()
    } else {
        vec![format!(
            "confirmed {:?} final target {} differs from published speed {}",
            seq, target, configured_speed
        )]
    })
}

fn analytic_parts(curve: &SpeedCurve) -> Result<Vec<(u64, ExponentialApproach)>, String> {
    let constant = |speed: f32| {
        ExponentialApproach::new(f64::from(speed), f64::from(speed), 0.0)
            .ok_or_else(|| "executed curve has an invalid constant speed".to_owned())
    };
    match curve {
        SpeedCurve::Constant(speed) => Ok(vec![(0, constant(*speed)?)]),
        SpeedCurve::Approach(law) => Ok(vec![(0, *law)]),
        SpeedCurve::ApproachSteps(parts) => Ok(parts.to_vec()),
        SpeedCurve::Steps(parts) => parts
            .iter()
            .map(|(at, speed)| Ok((*at, constant(*speed)?)))
            .collect(),
        SpeedCurve::Ramp { .. } => {
            Err("SYNC emitted a curve outside its analytic law contract".to_owned())
        }
    }
}

fn curve_advance(curve: &SpeedCurve, start: u64, end: u64) -> Result<f64, String> {
    let parts = analytic_parts(curve)?;
    let mut advance = 0.0;
    let mut cursor = start;
    for (index, (at, law)) in parts.iter().enumerate() {
        let boundary = parts.get(index + 1).map_or(end, |(next, _)| end.min(*next));
        if boundary <= cursor {
            continue;
        }
        let elapsed = cursor
            .checked_sub(*at)
            .ok_or_else(|| "executed curve does not cover its source anchor".to_owned())?;
        advance += law
            .advance_over(elapsed.as_(), (boundary - cursor).as_())
            .ok_or_else(|| "executed curve integral is not finite".to_owned())?;
        cursor = boundary;
        if cursor == end {
            break;
        }
    }
    if cursor != end {
        return Err("executed curve does not cover its consumed interval".to_owned());
    }
    Ok(advance)
}

fn curve_rate(curve: &SpeedCurve, frame: u64) -> Result<f64, String> {
    let parts = analytic_parts(curve)?;
    let (at, law) = parts
        .iter()
        .rev()
        .find(|(at, _)| *at <= frame)
        .ok_or_else(|| "executed curve does not cover the consumed frame".to_owned())?;
    let speed = law
        .rate_at((frame - at).as_())
        .ok_or_else(|| "executed rate is not finite".to_owned())?;
    Ok(speed)
}

fn consumed_media(slot: SlotSnapshot) -> Result<(SlotMark, Duration), String> {
    if slot.state != SlotState::Playing {
        return Err(format!(
            "law has no consumed playing slot: {:?}",
            slot.state
        ));
    }
    let mark = slot
        .mark
        .ok_or_else(|| "law has no consumed PCM mark".to_owned())?;
    let origin = slot
        .source
        .filter(|info| info.segment == mark.lane.segment)
        .and_then(|info| info.media_origin)
        .ok_or_else(|| "law has no known media origin".to_owned())?;
    let media = mark
        .position
        .checked_sub(origin)
        .ok_or_else(|| "law consumed raw position precedes media zero".to_owned())?;
    Ok((mark, media))
}

fn projected_media(
    observation: &LinkObservation,
    item: kithara::events::TrackId,
    curves: &[AppliedCurve<'_>],
    start: SlotSnapshot,
    end: SessionFrame,
    rate: NonZeroU32,
) -> Result<Duration, String> {
    let (mark, media) = consumed_media(start)?;
    let mut position = media.as_secs_f64();
    let initial = observation
        .speeds
        .iter()
        .find(|command| {
            command.item == item
                && matches!(
                    command.before.status,
                    kithara::play::TrackStatus::Playing { .. }
                )
                && command
                    .before
                    .mark
                    .is_some_and(|before| before.lane.segment == mark.lane.segment)
        })
        .map(|command| SpeedCurve::Constant(command.before.speed));
    let mut active = curves
        .iter()
        .rev()
        .find(|curve| curve.at <= mark.session)
        .map(|curve| (&curve.command.curve, curve.at));
    if active.is_none()
        && let Some(cue) = observation.cue_curves.iter().rev().find(|cue| {
            cue.item == item
                && observation.ready_cues.contains(&(item, cue.seq))
                && cue
                    .consumed
                    .and_then(|slot| slot.mark)
                    .is_some_and(|first| first.lane.segment == mark.lane.segment)
        })
    {
        let first = cue
            .consumed
            .and_then(|slot| slot.mark)
            .ok_or_else(|| "cue has no actual consumed Segment".to_owned())?;
        let frame = i64::from(first.session)
            .checked_sub(i64::try_from(first.lane.frame).map_err(|error| error.to_string())?)
            .map(SessionFrame::new)
            .ok_or_else(|| "cue lane/session translation overflowed".to_owned())?;
        active = Some((&cue.curve, frame));
    }
    let (mut curve, mut origin) = active
        .or_else(|| initial.as_ref().map(|curve| (curve, mark.session)))
        .ok_or_else(|| {
            "source projection has no actual initial speed or consumed cue law".to_owned()
        })?;
    let mut events = Vec::new();
    for applied in curves {
        if applied.at > mark.session && applied.at <= end {
            events.push((applied.at, applied.command.seq, Some(applied), None));
        }
        if let Some((to, at)) = applied.command.landing
            && at > mark.session
            && at <= end
            && !curves.iter().any(|replacement| {
                replacement.command.landing.is_some()
                    && replacement.command.seq != applied.command.seq
                    && (replacement.at > applied.at
                        || replacement.at == applied.at
                            && replacement.command.seq > applied.command.seq)
                    && replacement.at < at
            })
        {
            events.push((at, applied.command.seq, None, Some(to)));
        }
    }
    for placement in observation.placements.iter().filter(|placement| {
        placement.item == item
            && !placement.aligned
            && placement
                .consumed
                .mark
                .is_some_and(|before| before.lane.segment == mark.lane.segment)
    }) {
        if placement.at > mark.session && placement.at <= end {
            if !observation
                .placement_applied
                .contains(&(item, placement.seq, true))
            {
                return Err(format!(
                    "Jump {:?} has no actual Applied outcome",
                    placement.seq
                ));
            }
            events.push((placement.at, placement.seq, None, Some(placement.to)));
        }
    }
    events.sort_by_key(|(at, seq, _, _)| (*at, *seq));
    let mut cursor = mark.session;
    for (at, _, replacement, landing) in events {
        position += curve_advance(
            curve,
            cursor
                .frames_since(origin)
                .ok_or_else(|| "law origin is after consumption".to_owned())?,
            at.frames_since(origin)
                .ok_or_else(|| "law origin is after replacement".to_owned())?,
        )? / f64::from(rate.get());
        cursor = at;
        if let Some(replacement) = replacement {
            curve = &replacement.command.curve;
            origin = replacement.at;
        }
        if let Some(landing) = landing {
            position = landing.as_secs_f64();
        }
    }
    position += curve_advance(
        curve,
        cursor
            .frames_since(origin)
            .ok_or_else(|| "law origin is after consumption".to_owned())?,
        end.frames_since(origin)
            .ok_or_else(|| "law projection precedes its origin".to_owned())?,
    )? / f64::from(rate.get());
    Duration::try_from_secs_f64(position).map_err(|error| error.to_string())
}

fn grid_tempo(grid: &BeatGridSnapshot, position: MapPosition) -> Result<f64, String> {
    match grid.tempo_at(MapPoint::new(grid.stamp(), position)) {
        BeatGridQuery::Resolved(tempo) => Ok(f64::from(*tempo.value())),
        other => Err(format!(
            "law has no declared local tempo at {position:?}: {other:?}"
        )),
    }
}

fn media_seconds_position(seconds: f64, rate: NonZeroU32) -> Result<MapPosition, String> {
    AssetFrame::new(seconds * f64::from(rate.get()))
        .map(MapPosition::Asset)
        .map_err(|error| error.to_string())
}

fn frame_after(start: SessionFrame, offset: u64) -> Result<SessionFrame, String> {
    i64::from(start)
        .checked_add(i64::try_from(offset).map_err(|error| error.to_string())?)
        .map(SessionFrame::new)
        .ok_or_else(|| "source law leaves the session-frame axis".to_owned())
}

fn predecessor_rate(
    observation: &LinkObservation,
    item: kithara::events::TrackId,
    curves: &[AppliedCurve<'_>],
    index: usize,
) -> Result<f64, String> {
    let applied = &curves[index];
    if let Some(previous) = curves[..index]
        .iter()
        .rev()
        .find(|previous| previous.at <= applied.at)
    {
        return curve_rate(
            &previous.command.curve,
            applied
                .at
                .frames_since(previous.at)
                .ok_or_else(|| "predecessor law is after the actual Applied frame".to_owned())?,
        );
    }
    let mark = applied
        .command
        .before
        .mark
        .ok_or_else(|| "first law has no pre-command consumed mark".to_owned())?;
    if let Some(cue) = observation.cue_curves.iter().rev().find(|cue| {
        cue.item == item
            && observation.ready_cues.contains(&(item, cue.seq))
            && cue
                .consumed
                .and_then(|slot| slot.mark)
                .is_some_and(|first| first.lane.segment == mark.lane.segment)
    }) {
        let lane = mark
            .lane_at(applied.at)
            .ok_or_else(|| "cue predecessor cannot translate the real Applied frame".to_owned())?;
        return curve_rate(&cue.curve, lane.frame);
    }
    Ok(f64::from(applied.command.before.speed))
}

fn source_slope(grid: &BeatGridSnapshot, seconds: f64, rate: NonZeroU32) -> Result<f64, String> {
    Ok(60.0 / grid_tempo(grid, media_seconds_position(seconds, rate)?)?)
}

fn maximum_source_slope(
    model: &BeatGridModel,
    source: &BeatGridSnapshot,
    rate: NonZeroU32,
) -> Result<f64, String> {
    let observed = model
        .as_raw()
        .beats
        .first()
        .ok_or_else(|| "source slope has no observed beat".to_owned())?;
    let observed = Duration::try_from_secs_f64(observed.at).map_err(|error| error.to_string())?;
    let first = media_position(observed, rate)?;
    let MapPosition::Asset(first_frame) = first else {
        return Err("source slope has a different axis".to_owned());
    };
    let mut maximum = 60.0 / grid_tempo(source, first)?;
    for (start, _) in source_regions(source)? {
        let MapPosition::Asset(frame) = start else {
            return Err("source region has a different axis".to_owned());
        };
        if frame >= first_frame {
            maximum = maximum.max(60.0 / grid_tempo(source, start)?);
        }
    }
    if maximum.is_finite() && maximum > 0.0 {
        Ok(maximum)
    } else {
        Err("maximum source slope is not finite and positive".to_owned())
    }
}

fn declared_law(
    host: &TempoTrajectory,
    frame: SessionFrame,
    slope: f64,
    beat_offset: f64,
) -> Result<ExponentialApproach, String> {
    let anchor = host.anchor_at(frame);
    let reached = anchor.tempo_at(frame) + beat_offset;
    let target = anchor.target_beats_per_second() + beat_offset;
    ExponentialApproach::new(
        reached,
        target,
        if reached == target {
            0.0
        } else {
            anchor.smooth_seconds() * f64::from(host.sample_rate().get())
        },
    )
    .and_then(|law| law.scaled(slope))
    .ok_or_else(|| "declared source/Host law is not finite and playable".to_owned())
}

fn slew_offsets(
    from: f64,
    base: f64,
    rate: NonZeroU32,
    commanded: bool,
) -> Result<Vec<(u64, f64)>, String> {
    let epsilon = f64::from(LinkConfig::default().epsilon());
    let mut current = if commanded { base } else { from };
    let mut frame = 0_u64;
    let mut offsets = Vec::new();
    while current != base {
        let shift = (base - current).clamp(-epsilon, epsilon);
        let candidate = current + shift;
        let next = if (candidate - current).abs() <= epsilon {
            candidate
        } else if shift > 0.0 {
            candidate.next_down()
        } else {
            candidate.next_up()
        };
        if !next.is_finite() || next == current || (next - current).abs() > epsilon {
            return Err("actual predecessor has no representable epsilon-bounded slew".to_owned());
        }
        offsets.push((frame, next - base));
        frame = frame
            .checked_add(if next == base {
                1
            } else {
                u64::from(rate.get())
            })
            .ok_or_else(|| "declared slew does not fit the physical output axis".to_owned())?;
        current = next;
    }
    offsets.push((frame, 0.0));
    Ok(offsets)
}

fn host_progress(
    host: &TempoTrajectory,
    from: SessionFrame,
    to: SessionFrame,
) -> Result<f64, String> {
    let mut cursor = from;
    let mut beats = 0.0;
    for end in host
        .changes_after(from)
        .filter(|frame| *frame < to)
        .chain([to])
    {
        let frames = end
            .frames_since(cursor)
            .ok_or_else(|| "Host progress precedes its real anchor".to_owned())?;
        beats += declared_law(host, cursor, 1.0, 0.0)?
            .advance(frames.as_())
            .ok_or_else(|| "declared Host progress is not finite".to_owned())?
            / f64::from(host.sample_rate().get());
        cursor = end;
    }
    Ok(beats)
}

fn source_with_duration(
    model: &BeatGridModel,
    rate: NonZeroU32,
    duration: Option<Duration>,
) -> Result<BeatGridSnapshot, String> {
    let extent = match duration {
        Some(duration) => AssetExtent::Bounded(
            u64::try_from(
                AudioSpec::new(1, rate)
                    .frames_for(duration)
                    .map_err(|error| error.to_string())?
                    .get(),
            )
            .map_err(|error| error.to_string())?,
        ),
        None => AssetExtent::Unknown,
    };
    BeatGridSnapshot::model(
        BeatGridId::allocate().map_err(|error| error.to_string())?,
        BeatGridRevision::first(),
        model,
        AssetAxis::new(rate, extent),
    )
    .map_err(|error| error.to_string())
}

fn source_media_for_beat(
    grid: &BeatGridSnapshot,
    beat: f64,
    rate: NonZeroU32,
) -> Result<f64, String> {
    match grid.position_at(MapPoint::new(
        grid.stamp(),
        Beat::new(beat).map_err(|error| error.to_string())?,
    )) {
        BeatGridQuery::Resolved(position) => match position.value().value() {
            MapPosition::Asset(frame) => Ok(f64::from(*frame) / f64::from(rate.get())),
            _ => Err("declared inverse source geometry has a different axis".to_owned()),
        },
        other => Err(format!(
            "declared inverse source geometry cannot resolve beat {beat}: {other:?}"
        )),
    }
}

fn crossing_target(
    source: &BeatGridSnapshot,
    host: &TempoTrajectory,
    frame: SessionFrame,
    mut beat: f64,
    media_offset: Option<f64>,
    beat_offset: f64,
    rate: NonZeroU32,
) -> Result<(f64, f64), String> {
    let rate_value = f64::from(rate.get());
    if media_offset.is_none() {
        beat += declared_law(host, frame, 1.0, beat_offset)?
            .advance(1.0)
            .ok_or_else(|| "crossing beat integral is not finite".to_owned())?
            / rate_value;
        return Ok((source_media_for_beat(source, beat, rate)?, beat));
    }
    let offset =
        media_offset.ok_or_else(|| "media correction has no declared offset".to_owned())?;
    let host_law = declared_law(host, frame, 1.0, 0.0)?;
    let mut elapsed = 0.0;
    while elapsed < 1.0 {
        let position = source_media_for_beat(source, beat, rate)?;
        let point = media_seconds_position(position, rate)?;
        let BeatGridQuery::Resolved(kithara::warp::BeatGridRegion::Bounded(region)) =
            source.region_at(MapPoint::new(source.stamp(), point))
        else {
            return Err("crossing frame has no declared bounded source region".to_owned());
        };
        let slope = source_slope(source, position, rate)?;
        let boundary = grid_beat_at(source, region.end())?;
        let law = ExponentialApproach::new(
            host_law
                .rate_at(elapsed)
                .ok_or_else(|| "crossing Host rate is not finite".to_owned())?
                + offset / slope,
            host_law.target() + offset / slope,
            host_law.time_constant(),
        )
        .ok_or_else(|| "declared crossing beat law is not finite".to_owned())?;
        let remaining = 1.0 - elapsed;
        let through = law
            .advance(remaining)
            .ok_or_else(|| "crossing law integral is not finite".to_owned())?
            / rate_value;
        if beat + through <= boundary {
            beat += through;
            return Ok((source_media_for_beat(source, beat, rate)?, beat));
        }
        if boundary <= beat {
            return Err("source crossing leaves the supplied bounded geometry".to_owned());
        }
        let mut lower = 0.0;
        let mut upper = remaining;
        loop {
            let middle = lower + (upper - lower) * 0.5;
            if middle == lower || middle == upper {
                break;
            }
            if law
                .advance(middle)
                .ok_or_else(|| "crossing law is not finite".to_owned())?
                / rate_value
                < boundary - beat
            {
                lower = middle;
            } else {
                upper = middle;
            }
        }
        let next = elapsed + upper;
        if next <= elapsed {
            return Err("source crossing does not advance within its physical frame".to_owned());
        }
        elapsed = next;
        beat = boundary;
    }
    Ok((source_media_for_beat(source, beat, rate)?, beat))
}

fn ordinary_span_is_local(
    source: &BeatGridSnapshot,
    from: f64,
    to: f64,
    rate: NonZeroU32,
    include_endpoints: bool,
) -> Result<bool, String> {
    let position = source_media_for_beat(source, from, rate)?;
    let mut cursor = media_seconds_position(position, rate)?;
    let mut tempo = grid_tempo(source, cursor)?;
    let BeatGridQuery::Resolved(kithara::warp::BeatGridRegion::Bounded(region)) =
        source.region_at(MapPoint::new(source.stamp(), cursor))
    else {
        return Err("source span has no declared bounded region".to_owned());
    };
    let boundary = grid_beat_at(source, region.start())?;
    if let MapPosition::Asset(start) = region.start()
        && f64::from(start) > 0.0
        && from <= boundary
        && boundary <= to
        && (include_endpoints || from < boundary && boundary < to)
    {
        let previous = MapPosition::Asset(
            AssetFrame::new(f64::from(start).next_down()).map_err(|error| error.to_string())?,
        );
        if grid_tempo(source, previous)? != tempo {
            return Ok(false);
        }
    }
    loop {
        let BeatGridQuery::Resolved(kithara::warp::BeatGridRegion::Bounded(region)) =
            source.region_at(MapPoint::new(source.stamp(), cursor))
        else {
            return Err("source span has no declared bounded region".to_owned());
        };
        let boundary = grid_beat_at(source, region.end())?;
        if boundary > to || !include_endpoints && boundary == to {
            return Ok(true);
        }
        if region.end() == cursor {
            return if boundary == to {
                Ok(true)
            } else {
                Err("source span leaves the supplied bounded geometry".to_owned())
            };
        }
        cursor = region.end();
        let next = grid_tempo(source, cursor)?;
        if boundary >= from && (include_endpoints || boundary > from) && next != tempo {
            return Ok(false);
        }
        tempo = next;
    }
}

fn cue_prefix(
    cue: &LinkCue,
    host: &TempoTrajectory,
    source: &BeatGridSnapshot,
    through: u64,
    rate: NonZeroU32,
    failures: &mut Vec<String>,
) -> Result<SelectedBinding, String> {
    let (first, media) = consumed_media(
        cue.consumed
            .ok_or_else(|| "ready cue has no actually consumed Segment".to_owned())?,
    )?;
    let origin = i64::from(first.session)
        .checked_sub(i64::try_from(first.lane.frame).map_err(|error| error.to_string())?)
        .map(SessionFrame::new)
        .ok_or_else(|| "cue lane/session translation overflowed".to_owned())?;
    let first_expected = cue.position.as_secs_f64()
        + curve_advance(&cue.curve, 0, first.lane.frame)? / f64::from(rate.get());
    if (first_expected * f64::from(rate.get())).round()
        != (media.as_secs_f64() * f64::from(rate.get())).round()
    {
        failures.push(format!(
            "cue {:?} position/law predicts asset frame {}, actual first Segment consumes {}",
            cue.seq,
            first_expected * f64::from(rate.get()),
            media.as_secs_f64() * f64::from(rate.get())
        ));
    }
    let parts = analytic_parts(&cue.curve)?;
    let mut position = cue.position.as_secs_f64();
    let mut beat = grid_beat_at(source, media_seconds_position(position, rate)?)?;
    for (index, &(at, law)) in parts
        .iter()
        .enumerate()
        .take_while(|(_, (at, _))| *at < through)
    {
        let end = parts
            .get(index + 1)
            .map_or(through, |(next, _)| (*next).min(through));
        let frames = end - at;
        let frame = frame_after(origin, at)?;
        let slope = source_slope(source, source_media_for_beat(source, beat, rate)?, rate)?;
        let next_beat = beat
            + declared_law(host, frame, 1.0, 0.0)?
                .advance(frames.as_())
                .ok_or_else(|| "cue beat progress is not finite".to_owned())?
                / f64::from(rate.get());
        let advance = law
            .advance(frames.as_())
            .ok_or_else(|| "cue analytic integral is not finite".to_owned())?
            / f64::from(rate.get());
        let bridge = frames == 1
            && law.time_constant() == 0.0
            && !ordinary_span_is_local(source, beat, next_beat, rate, true)?;
        if bridge {
            let (expected, next_beat) =
                crossing_target(source, host, frame, beat, None, 0.0, rate)?;
            beat = next_beat;
            if ((position + advance) * f64::from(rate.get())).round()
                != (expected * f64::from(rate.get())).round()
            {
                failures.push(format!("cue {:?} frame {at}: crossing consumes {}, source/Host inverse geometry requires {}", cue.seq,
                    (position + advance) * f64::from(rate.get()), expected * f64::from(rate.get())));
            }
        } else {
            let expected = declared_law(host, frame, slope, 0.0)?;
            if law != expected {
                failures.push(format!("cue {:?} frame {at}: analytic law {:?} differs from declared source/Host law {:?}", cue.seq, law, expected));
            }
            if !ordinary_span_is_local(source, beat, next_beat, rate, false)? {
                failures.push(format!(
                    "cue {:?} frame {at}: ordinary law crosses a source slope",
                    cue.seq
                ));
            }
        }
        if !bridge {
            beat = next_beat;
        }
        position += advance;
    }
    declared_binding_at(source, host, beat, frame_after(origin, through)?)
}

fn owned_host_history(
    observation: &LinkObservation,
    rate: NonZeroU32,
) -> Result<(usize, &TempoTrajectory), String> {
    let (host_history_input, host_history) = observation
        .owner_inputs
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, input)| input.trajectory.as_ref().map(|host| (index, host)))
        .ok_or_else(|| "law has no real complete Host trajectory input".to_owned())?;
    if host_history.sample_rate() != rate {
        return Err("published Host proof and consumed source have different clocks".to_owned());
    }
    Ok((host_history_input, host_history))
}

pub(super) fn published_host_history(
    observation: &LinkObservation,
    host_grids: &[(SessionFrame, BeatGridSnapshot)],
    rate: NonZeroU32,
    failures: &mut Vec<String>,
) -> Result<usize, String> {
    if host_grids.is_empty() {
        return Err("law has no actual published Host execution history".to_owned());
    }
    let (host_history_input, host_history) = owned_host_history(observation, rate)?;
    for (frame, published) in host_grids {
        let expected_beat = f64::from(host_history.beat_at(*frame));
        let actual_beat = grid_beat_at(published, MapPosition::Session(*frame))?;
        if session_frame_at(published, expected_beat)? != *frame
            || grid_tempo(published, MapPosition::Session(*frame))?
                != host_history.anchor_at(*frame).tempo_at(*frame) * 60.0
        {
            failures.push(format!("Host execution at {:?}: complete owner trajectory beat {} differs from actual published beat {} or reached tempo", frame, expected_beat, actual_beat));
        }
    }
    Ok(host_history_input)
}

fn consumed_cue_law(
    model: &BeatGridModel,
    observation: &LinkObservation,
    curves: &[AppliedCurve<'_>],
    snapshot: &kithara::link::LinkedSnapshot<kithara::play::TrackSnapshot>,
    rate: NonZeroU32,
    failures: &mut Vec<String>,
) -> Result<Option<SelectedBinding>, String> {
    let item = snapshot.track.item;
    let mark = snapshot
        .track
        .mark
        .ok_or_else(|| "published source has no consumed mark".to_owned())?;
    if let Some(cue) = observation.cue_curves.iter().rev().find(|cue| {
        cue.item == item
            && observation.ready_cues.contains(&(item, cue.seq))
            && cue
                .consumed
                .and_then(|slot| slot.mark)
                .is_some_and(|first| first.lane.segment == mark.lane.segment)
    }) {
        let first = cue
            .consumed
            .and_then(|slot| slot.mark)
            .ok_or_else(|| "owned cue lost its consumed Segment".to_owned())?;
        let origin = i64::from(first.session)
            .checked_sub(i64::try_from(first.lane.frame).map_err(|error| error.to_string())?)
            .map(SessionFrame::new)
            .ok_or_else(|| "cue lane/session translation overflowed".to_owned())?;
        let host = cue
            .host_input
            .and_then(|input| observation.owner_inputs.get(input))
            .and_then(|input| input.trajectory.as_ref())
            .ok_or_else(|| "owned consumed cue has no real Host input".to_owned())?;
        let source = source_with_duration(model, rate, cue.before.duration)?;
        let through = curves
            .first()
            .map_or(mark.session, |first| first.at.min(mark.session))
            .frames_since(origin)
            .ok_or_else(|| {
                "cue replacement precedes its consumed lane/session origin".to_owned()
            })?;
        return cue_prefix(cue, host, &source, through, rate, failures).map(Some);
    }
    Ok(None)
}

impl AppliedCurve<'_> {
    fn planned_input(&self) -> Result<&(Duration, f64), String> {
        self.command
            .planned
            .as_ref()
            .ok_or_else(|| "Applied law has no recorded real planned(C) input".to_owned())?
            .as_ref()
            .map_err(|error| format!("Applied planned(C) failed: {error}"))
    }

    fn owner_host<'b>(
        &self,
        observation: &'b LinkObservation,
    ) -> Result<&'b TempoTrajectory, String> {
        let command = self.command;
        let rate = observation
            .consumed_rate
            .ok_or_else(|| "Applied law has no consumed evaluation clock".to_owned())?;
        let host = command
            .host_input
            .and_then(|input| observation.owner_inputs.get(input))
            .and_then(|input| input.trajectory.as_ref())
            .ok_or_else(|| {
                format!(
                    "Applied {:?} has no real owner Host trajectory input",
                    command.seq
                )
            })?;
        if host.sample_rate() != rate {
            return Err("owner Host trajectory and consumed law have different clocks".to_owned());
        }
        Ok(host)
    }

    fn check_planned_input(
        &self,
        observation: &LinkObservation,
        curves: &[AppliedCurve<'_>],
        index: usize,
        failures: &mut Vec<String>,
    ) -> Result<f64, String> {
        let command = self.command;
        let item = command.item;
        let rate = observation
            .consumed_rate
            .ok_or_else(|| "Applied law has no consumed evaluation clock".to_owned())?;
        let before = command
            .consumed
            .ok_or_else(|| "Applied law has no consumed source anchor".to_owned())?;
        let replayed_media =
            projected_media(observation, item, &curves[..index], before, self.at, rate)?;
        let predecessor = predecessor_rate(observation, item, curves, index)?;
        let planned = self.planned_input()?;
        if (planned.0.as_secs_f64() * f64::from(rate.get())).round()
            != (replayed_media.as_secs_f64() * f64::from(rate.get())).round()
        {
            failures.push(format!(
                "Applied {:?} planned(C) position {:?} differs from prior Applied replay {:?}",
                command.seq, planned.0, replayed_media
            ));
        }
        if planned.1 != predecessor {
            failures.push(format!(
                "Applied {:?} planned(C) velocity {} differs from real predecessor {}",
                command.seq, planned.1, predecessor
            ));
        }
        Ok(predecessor)
    }

    fn prefix_boundaries(
        &self,
        host: &TempoTrajectory,
        horizon: SessionFrame,
        parts: &[(u64, ExponentialApproach)],
        offsets: &[(u64, f64)],
        terminal: u64,
    ) -> Result<Vec<u64>, String> {
        let command = self.command;
        let limit = horizon
            .frames_since(self.at)
            .ok_or_else(|| "executed replacement precedes Applied".to_owned())?;
        let mut boundaries: Vec<_> = parts
            .iter()
            .map(|(at, _)| *at)
            .filter(|at| *at <= limit)
            .collect();
        boundaries.extend(offsets.iter().map(|(at, _)| *at).filter(|at| *at <= limit));
        if terminal <= limit {
            boundaries.push(terminal);
        }
        boundaries.extend(
            host.changes_after(self.at)
                .filter_map(|at| at.frames_since(self.at))
                .filter(|at| *at <= limit),
        );
        if let Some((_, landing)) = command.landing {
            boundaries.push(
                landing
                    .frames_since(self.at)
                    .ok_or_else(|| "Align landing precedes C".to_owned())?
                    .min(limit),
            );
        }
        boundaries.push(limit);
        boundaries.sort_unstable();
        boundaries.dedup();
        Ok(boundaries)
    }

    fn source_phase(
        &self,
        model: &BeatGridModel,
        host: &TempoTrajectory,
        initial_beat: f64,
    ) -> Result<f64, String> {
        let (meter_origin, source_stride, host_stride) = phase_axes(model, 4)?;
        let phase = ((initial_beat - meter_origin).rem_euclid(source_stride)
            - f64::from(host.beat_at(self.at)).rem_euclid(host_stride))
        .rem_euclid(host_stride)
        .rem_euclid(1.0);
        let phase = if phase > 0.5 { phase - 1.0 } else { phase };
        Ok(phase)
    }

    fn terminal_offset(
        &self,
        host: &TempoTrajectory,
        source_progress: f64,
        frame: SessionFrame,
        terminal: u64,
        maximum_slope: f64,
        failures: &mut Vec<String>,
    ) -> Result<(u64, f64), String> {
        let residual = source_progress - host_progress(host, self.at, frame)?;
        let length = (residual.abs() * f64::from(host.sample_rate().get()) * maximum_slope
            / f64::from(LinkConfig::default().epsilon()))
        .ceil()
        .to_u64()
        .ok_or_else(|| "declared terminal duration is unrepresentable".to_owned())?;
        let deadline = terminal
            .checked_add(length)
            .ok_or_else(|| "declared correction deadline overflows".to_owned())?;
        if length == 0 && residual != 0.0 {
            failures.push(format!(
                "Applied {:?} declares zero terminal duration with beat residual {}",
                self.command.seq, residual
            ));
        }
        let length: f64 = length.as_();
        Ok((
            deadline,
            if length == 0.0 {
                0.0
            } else {
                -residual * f64::from(host.sample_rate().get()) / length
            },
        ))
    }

    fn ordinary_coefficients(
        &self,
        source: &BeatGridSnapshot,
        host: &TempoTrajectory,
        prefixes: &[(u64, f64)],
        plan: &PrefixPlan,
        q: Option<f64>,
    ) -> Result<Option<String>, String> {
        let command = self.command;
        let rate = host.sample_rate();
        let at = prefixes
            .last()
            .ok_or_else(|| "ordinary law has no checked prefix".to_owned())?
            .0;
        let parts = analytic_parts(&command.curve)?;
        let (law_at, law) = parts
            .iter()
            .rev()
            .find(|(law_at, _)| *law_at <= at)
            .ok_or_else(|| {
                "Applied law has no analytic interval at the executed frame".to_owned()
            })?;
        let correcting = command.landing.is_none();
        let terminal = plan.terminal;
        let deadline = plan.deadline;
        let original_position = prefixes
            .iter()
            .find_map(|(at, position)| (*at == *law_at).then_some(*position))
            .ok_or_else(|| {
                "ordinary law has no independently checked original source anchor".to_owned()
            })?;
        let original_slope = source_slope(source, original_position, rate)?;
        let original_offset = if correcting && *law_at < terminal {
            plan.offsets
                .iter()
                .rev()
                .find(|(at, _)| *at <= *law_at)
                .ok_or_else(|| "ordinary head law has no declared step".to_owned())?
                .1
                / original_slope
        } else if deadline.is_some_and(|end| *law_at < end) && *law_at >= terminal {
            q.ok_or_else(|| "ordinary terminal law has no independently checked prefix".to_owned())?
        } else {
            0.0
        };
        let original = declared_law(
            host,
            frame_after(self.at, *law_at)?,
            original_slope,
            original_offset,
        )?;
        Ok((*law != original).then(|| format!("Applied {:?} frame {law_at}: analytic law {:?} differs from declared source/Host law {:?}", command.seq, law, original)))
    }

    fn check_partial_area(
        &self,
        expected: ExponentialApproach,
        position: f64,
        advance: f64,
        interval: std::ops::Range<u64>,
        host: &TempoTrajectory,
        failures: &mut Vec<String>,
    ) -> Result<(), String> {
        let command = self.command;
        let frames = interval.end - interval.start;
        let at = interval.start;
        let rate = host.sample_rate();
        let expected_position = position
            + expected
                .advance(frames.as_())
                .ok_or_else(|| "declared partial area is not finite".to_owned())?
                / f64::from(rate.get());
        if ((position + advance) * f64::from(rate.get())).round()
            != (expected_position * f64::from(rate.get())).round()
        {
            failures.push(format!("Applied {:?} frame {at}: partial area differs from the declared physical source frame", command.seq));
        }
        Ok(())
    }

    fn check_epsilon(
        &self,
        previous_offset: f64,
        offset: f64,
        at: u64,
        failures: &mut Vec<String>,
    ) {
        let command = self.command;
        let epsilon = f64::from(LinkConfig::default().epsilon());
        if (offset - previous_offset).abs() > epsilon {
            failures.push(format!(
                "Applied {:?} frame {at}: correction media-speed step {} exceeds epsilon {}",
                command.seq,
                (offset - previous_offset).abs(),
                epsilon
            ));
        }
    }

    fn interval_offsets(
        &self,
        plan: &PrefixPlan,
        q: Option<f64>,
        at: u64,
    ) -> Result<(Option<f64>, f64), String> {
        let correcting = self.command.landing.is_none();
        let terminal = plan.terminal;
        let deadline = plan.deadline;
        let media_offset = if correcting && at < terminal {
            Some(
                plan.offsets
                    .iter()
                    .rev()
                    .find(|(starts, _)| *starts <= at)
                    .ok_or_else(|| "head slew has no declared step".to_owned())?
                    .1,
            )
        } else {
            None
        };
        let beat_offset = if deadline.is_some_and(|end| at < end) && at >= terminal {
            q.ok_or_else(|| {
                "terminal correction began without an independently checked prefix".to_owned()
            })?
        } else {
            0.0
        };
        Ok((media_offset, beat_offset))
    }

    fn interval_beat(
        &self,
        host: &TempoTrajectory,
        beat: f64,
        slope: f64,
        interval: std::ops::Range<u64>,
        media_offset: Option<f64>,
        beat_offset: f64,
    ) -> Result<f64, String> {
        let rate = host.sample_rate();
        let frame = frame_after(self.at, interval.start)?;
        let frames = interval.end - interval.start;
        let next_beat = beat
            + declared_law(
                host,
                frame,
                1.0,
                media_offset.map_or(beat_offset, |offset| offset / slope),
            )?
            .advance(frames.as_())
            .ok_or_else(|| "validated prefix beat progress is not finite".to_owned())?
                / f64::from(rate.get());
        Ok(next_beat)
    }

    fn prefix_plan(
        &self,
        source: &BeatGridSnapshot,
        host: &TempoTrajectory,
        horizon: std::ops::Bound<SessionFrame>,
        predecessor: f64,
        commanded: bool,
    ) -> Result<PrefixPlan, String> {
        let command = self.command;
        let rate = host.sample_rate();
        let (horizon, latest) = match horizon {
            std::ops::Bound::Included(frame) => (frame, true),
            std::ops::Bound::Excluded(frame) => (frame, false),
            std::ops::Bound::Unbounded => {
                return Err("source law proof has no execution horizon".to_owned());
            }
        };
        let initial_media = self.planned_input()?.0;
        let base = host.anchor_at(self.at).tempo_at(self.at)
            * source_slope(source, initial_media.as_secs_f64(), rate)?;
        let correcting = command.landing.is_none();
        let offsets = if correcting {
            slew_offsets(predecessor, base, rate, commanded)?
        } else {
            vec![(0, 0.0)]
        };
        let last = offsets
            .last()
            .ok_or_else(|| "declared slew is empty".to_owned())?
            .0;
        let terminal = if commanded && correcting {
            last.max(1)
        } else {
            last
        };
        let parts = analytic_parts(&command.curve)?;
        let deadline = None;
        let limit = horizon
            .frames_since(self.at)
            .ok_or_else(|| "executed replacement precedes Applied".to_owned())?;
        let boundaries = self.prefix_boundaries(host, horizon, &parts, &offsets, terminal)?;
        let previous_offset = if commanded || !correcting {
            0.0
        } else {
            predecessor - base
        };
        Ok(PrefixPlan {
            latest,
            initial_media,
            offsets,
            terminal,
            parts,
            deadline,
            limit,
            boundaries,
            previous_offset,
        })
    }

    fn verify_prefix(
        &self,
        geometry: &SourceGeometry<'_>,
        host: &TempoTrajectory,
        horizon: std::ops::Bound<SessionFrame>,
        predecessor: f64,
        commanded: bool,
    ) -> Result<PrefixProof, String> {
        let model = geometry.model;
        let source = &geometry.source;
        let command = self.command;
        let rate = host.sample_rate();
        let mut plan = self.prefix_plan(source, host, horizon, predecessor, commanded)?;
        let mut previous_offset = plan.previous_offset;
        let correcting = command.landing.is_none();
        let mut failures = Vec::new();
        let mut position = plan.initial_media.as_secs_f64();
        let initial_beat = grid_beat_at(source, media_seconds_position(position, rate)?)?;
        let mut beat = initial_beat;
        let phase = self.source_phase(model, host, initial_beat)?;
        let mut q = None;
        let mut prefixes = Vec::new();
        let mut diagnostics = Vec::new();
        let mut boundary_index = 0;
        while let Some(&at) = plan.boundaries.get(boundary_index) {
            boundary_index += 1;
            if !plan.latest && at == plan.limit {
                break;
            }
            let frame = frame_after(self.at, at)?;
            let before_failures = failures.len();
            if let Some((landing, landing_at)) = command.landing
                && frame == landing_at
            {
                position = landing.as_secs_f64();
                beat = grid_beat_at(source, media_seconds_position(position, rate)?)?;
            }
            prefixes.push((at, source_media_for_beat(source, beat, rate)?));
            if correcting && at == plan.terminal {
                let (deadline, offset) = self.terminal_offset(
                    host,
                    phase + beat - initial_beat,
                    frame,
                    plan.terminal,
                    geometry.maximum_slope,
                    &mut failures,
                )?;
                plan.deadline = Some(deadline);
                q = Some(offset);
                if deadline <= plan.limit
                    && let Err(index) = plan.boundaries.binary_search(&deadline)
                {
                    plan.boundaries.insert(index, deadline);
                }
            }
            let end = plan.boundaries.get(boundary_index).copied().unwrap_or(at);
            let frames = end - at;
            if frames == 0 {
                continue;
            }
            let (law_at, law) = plan
                .parts
                .iter()
                .rev()
                .find(|(law_at, _)| *law_at <= at)
                .ok_or_else(|| {
                    "Applied law has no analytic interval at the executed frame".to_owned()
                })?;
            let law_elapsed = at - law_at;
            let advance = law
                .advance_over(law_elapsed.as_(), frames.as_())
                .ok_or_else(|| "Applied law has an invalid analytic integral".to_owned())?
                / f64::from(rate.get());
            let slope = source_slope(source, source_media_for_beat(source, beat, rate)?, rate)?;
            let (media_offset, beat_offset) = self.interval_offsets(&plan, q, at)?;
            let offset = media_offset.unwrap_or(beat_offset * slope);
            let next_beat =
                self.interval_beat(host, beat, slope, at..end, media_offset, beat_offset)?;
            let bridge = frames == 1
                && law_elapsed == 0
                && law.time_constant() == 0.0
                && !ordinary_span_is_local(source, beat, next_beat, rate, true)?;
            let from_beat = beat;
            if bridge {
                let (expected, next_beat) =
                    crossing_target(source, host, frame, beat, media_offset, beat_offset, rate)?;
                beat = next_beat;
                if ((position + advance) * f64::from(rate.get())).round()
                    != (expected * f64::from(rate.get())).round()
                {
                    failures.push(format!("Applied {:?} frame {at}: crossing consumes asset frame {}, declared inverse geometry requires {}", command.seq,
                        (position + advance) * f64::from(rate.get()), expected * f64::from(rate.get())));
                }
            } else {
                let expected = declared_law(
                    host,
                    frame,
                    slope,
                    media_offset.map_or(beat_offset, |offset| offset / slope),
                )?;
                if let Some(failure) =
                    self.ordinary_coefficients(source, host, &prefixes, &plan, q)?
                {
                    failures.push(failure);
                }
                self.check_partial_area(expected, position, advance, at..end, host, &mut failures)?;
                if !ordinary_span_is_local(source, beat, next_beat, rate, false)? {
                    failures.push(format!("Applied {:?} frame {at}: an ordinary analytic interval crosses the declared source slope", command.seq));
                }
                beat = next_beat;
            }
            if correcting {
                self.check_epsilon(previous_offset, offset, at, &mut failures);
            }
            if failures.len() > before_failures {
                let from_position = source_media_for_beat(source, from_beat, rate)
                    .and_then(|seconds| media_seconds_position(seconds, rate));
                let to_position = source_media_for_beat(source, beat, rate)
                    .and_then(|seconds| media_seconds_position(seconds, rate));
                let from_region = from_position
                    .as_ref()
                    .map(|position| source.region_at(MapPoint::new(source.stamp(), *position)));
                let to_region = to_position
                    .as_ref()
                    .map(|position| source.region_at(MapPoint::new(source.stamp(), *position)));
                diagnostics.push(serde_json::json!({
                    "at": at, "end": end, "frames": frames, "limit": plan.limit,
                    "law_at": law_at, "law_elapsed": law_elapsed,
                    "from_beat": from_beat, "to_beat": beat,
                    "from_position": format!("{from_position:?}"),
                    "to_position": format!("{to_position:?}"),
                    "from_region": format!("{from_region:?}"),
                    "to_region": format!("{to_region:?}"),
                    "slope": slope, "media_offset": media_offset, "beat_offset": beat_offset,
                    "bridge": bridge,
                    "local_including_endpoints": format!("{:?}", ordinary_span_is_local(source, from_beat, beat, rate, true)),
                    "local_excluding_endpoints": format!("{:?}", ordinary_span_is_local(source, from_beat, beat, rate, false)),
                    "checked_prefixes": prefixes,
                    "emitted_laws": plan.parts.iter().map(|(at, law)| serde_json::json!({
                        "at": at, "initial": law.initial(), "target": law.target(),
                        "time_constant": law.time_constant(),
                    })).collect::<Vec<_>>(),
                    "failures": &failures[before_failures..],
                }));
            }
            previous_offset = offset;
            position += advance;
        }
        let reference = declared_binding_at(source, host, beat, frame_after(self.at, plan.limit)?)?;
        Ok((
            plan.terminal,
            plan.deadline,
            failures,
            diagnostics,
            reference,
        ))
    }

    fn verify_end(
        &self,
        geometry: &SourceGeometry<'_>,
        observation: &LinkObservation,
        curves: &[AppliedCurve<'_>],
        snapshot: &kithara::link::LinkedSnapshot<kithara::play::TrackSnapshot>,
        deadline: Option<u64>,
    ) -> Result<Vec<String>, String> {
        let model = geometry.model;
        let source = &geometry.source;
        let command = self.command;
        let item = command.item;
        let host = self.owner_host(observation)?;
        let rate = host.sample_rate();
        let mark = snapshot
            .track
            .mark
            .ok_or_else(|| "published source has no consumed mark".to_owned())?;
        let claimed_end = match snapshot.sync {
            kithara::link::SyncStatus::On => None,
            kithara::link::SyncStatus::Correcting { remaining } => Some(mark.session + remaining),
            other => {
                return Err(format!(
                    "playing SYNC has no executed phase contract: {other:?}"
                ));
            }
        };
        let correcting = command.landing.is_none();
        if correcting && deadline.is_none() {
            return Err(
                "latest Applied correction has no independently checked terminal".to_owned(),
            );
        }
        let before = command
            .consumed
            .ok_or_else(|| "Applied law has no consumed source anchor".to_owned())?;
        let mut failures = Vec::new();
        if let Some(end) = claimed_end {
            if !correcting {
                return Err(
                    "Correcting refers to an Align relocation rather than a finite correction"
                        .to_owned(),
                );
            }
            if deadline
                .map(|offset| frame_after(self.at, offset))
                .transpose()?
                != Some(end)
            {
                failures.push(format!("Correcting deadline {:?} differs from its independently declared phase correction end", end));
            }
        } else if deadline
            .is_some_and(|end| frame_after(self.at, end).is_ok_and(|end| end > mark.session))
        {
            failures.push(format!(
                "On is published before Applied {:?} completes its finite phase correction",
                command.seq
            ));
        }
        if let Some(deadline) = deadline {
            let end = frame_after(self.at, deadline)?;
            let landing = projected_media(observation, item, curves, before, end, rate)?;
            let phase =
                grid_beat_at(source, media_position(landing, rate)?)? - phase_axes(model, 4)?.0;
            let host_phase = f64::from(host.beat_at(end));
            let closed = host.frame_at(
                SessionBeat::new(phase - (phase - host_phase).round())
                    .map_err(|error| error.to_string())?,
            );
            if closed != end {
                failures.push(format!(
                    "finite correction ends at {:?}, declared beat phase closes at {:?}",
                    end, closed
                ));
            }
        }
        Ok(failures)
    }

    fn verify(
        &self,
        geometry: &SourceGeometry<'_>,
        observation: &LinkObservation,
        curves: &[AppliedCurve<'_>],
        index: usize,
        snapshot: &kithara::link::LinkedSnapshot<kithara::play::TrackSnapshot>,
        failures: &mut Vec<String>,
    ) -> Result<(serde_json::Value, SelectedBinding), String> {
        let command = self.command;
        let host = self.owner_host(observation)?;
        let input = command
            .input
            .and_then(|input| observation.owner_inputs.get(input))
            .ok_or_else(|| {
                format!(
                    "Applied {:?} has no typed owner operation input",
                    command.seq
                )
            })?;
        let commanded = matches!(input.kind, LinkOwnerInputKind::Retime);
        if commanded && input.at != Some(self.at) {
            failures.push(format!(
                "Applied {:?} retime frame {:?} differs from its owner input {:?}",
                command.seq, self.at, input.at
            ));
        }
        let predecessor = self.check_planned_input(observation, curves, index, failures)?;
        let mark = snapshot
            .track
            .mark
            .ok_or_else(|| "published source has no consumed mark".to_owned())?;
        let claimed_end = match snapshot.sync {
            kithara::link::SyncStatus::On => None,
            kithara::link::SyncStatus::Correcting { remaining } => Some(mark.session + remaining),
            other => {
                return Err(format!(
                    "playing SYNC has no executed phase contract: {other:?}"
                ));
            }
        };
        let latest = curves.iter().rposition(|curve| curve.at <= mark.session) == Some(index);
        let replacement = curves.get(index + 1).map(|next| next.at);
        let horizon = if replacement.is_some_and(|next| next <= mark.session) {
            replacement.ok_or_else(|| "replacement disappeared".to_owned())?
        } else {
            claimed_end.unwrap_or(mark.session).max(mark.session)
        };
        let bound = if latest {
            std::ops::Bound::Included(horizon)
        } else {
            std::ops::Bound::Excluded(horizon)
        };
        let (terminal, deadline, prefix_failures, prefix_diagnostics, reference) =
            self.verify_prefix(geometry, host, bound, predecessor, commanded)?;
        failures.extend(prefix_failures);
        if latest {
            failures.extend(self.verify_end(geometry, observation, curves, snapshot, deadline)?);
        }
        Ok((
            serde_json::json!({"seq": command.seq.get(), "applied": i64::from(self.at), "verified_through": i64::from(horizon), "terminal_start": terminal, "terminal_end": deadline, "host_input": command.host_input, "prefix_diagnostics": prefix_diagnostics}),
            reference,
        ))
    }
}

pub(super) fn commanded_span_failures(
    command: &LinkSpeed,
    issued: SessionFrame,
    at: SessionFrame,
    marks: &[(SessionFrame, SlotSnapshot)],
    jump: Option<(Duration, SessionFrame)>,
    decoded: &super::control::DecodedCapture,
) -> Result<Vec<String>, String> {
    let (_, first) = marks
        .first()
        .ok_or_else(|| "command law has no initial PCM".to_owned())?;
    let mark = first
        .mark
        .ok_or_else(|| "command law has no initial mark".to_owned())?;
    let info = first
        .source
        .ok_or_else(|| "command law has no initial source".to_owned())?;
    let rate = info.spec.sample_rate;
    let initial = info
        .source_span
        .filter(|span| span.sample_rate() == rate)
        .and_then(|span| span.source_ratio_at(mark.lane.frame.checked_sub(info.lane_frame)?))
        .ok_or_else(|| "command law has no exact initial source coordinate".to_owned())?;
    let (input_origin, input) = decoded.physical_basis(info, initial)?;
    let initial = input
        .output_ratio_at_source(input_origin, initial)
        .ok_or_else(|| "command law cannot invert its decoder input clock".to_owned())?;
    let native_input = input
        .scaled_from(
            (0, NonZeroU128::MIN),
            (0, NonZeroU128::MIN),
            (
                u128::from(input.sample_rate().get()),
                NonZeroU128::from(rate),
            ),
        )
        .ok_or_else(|| "command law cannot restore its decoder source units".to_owned())?;
    if command.before.speed != 1.0 {
        return Err("latency fixture must issue from settled unity PCM".to_owned());
    }
    let end = marks
        .last()
        .ok_or_else(|| "command law has no final PCM".to_owned())?
        .0;
    let constant =
        ExponentialApproach::new(1.0, 1.0, 0.0).ok_or_else(|| "unity law is invalid".to_owned())?;
    let mut events = vec![(issued, constant, None)];
    for (offset, law) in analytic_parts(&command.curve)? {
        events.push((frame_after(at, offset)?, law, None));
    }
    if let Some((media, landing)) = jump {
        let law = events
            .iter()
            .rev()
            .find(|(frame, _, _)| *frame <= landing)
            .ok_or_else(|| "jump has no admitted law".to_owned())?
            .1;
        if law.initial() != 1.0 || law.target() != 1.0 {
            return Err("running SYNC fixture must jump at unity speed".to_owned());
        }
        let raw = media
            .checked_add(
                info.media_origin
                    .ok_or_else(|| "jump law has no immutable media origin".to_owned())?,
            )
            .ok_or_else(|| "jump raw coordinate overflows".to_owned())?;
        let numerator = raw
            .as_nanos()
            .checked_mul(u128::from(rate.get()))
            .ok_or_else(|| "jump raw source coordinate overflows".to_owned())?;
        let position = (
            numerator,
            NonZeroU128::new(1_000_000_000).expect("nanosecond source clock"),
        );
        let position = input
            .output_ratio_at_source(input_origin, position)
            .ok_or_else(|| "jump law cannot invert its decoder input clock".to_owned())?;
        events.push((landing, law, Some(position)));
    }
    events.sort_by_key(|(frame, _, _)| *frame);
    let mut expected = Vec::new();
    let mut position = initial;
    for (index, &(start, law, reset)) in events.iter().enumerate() {
        if start >= end {
            break;
        }
        if let Some(reset) = reset {
            position = reset;
        }
        if start == at {
            position = physical_phase(position)?;
        }
        let stop = events
            .get(index + 1)
            .map_or(end, |(frame, _, _)| (*frame).min(end));
        let frames = stop
            .frames_since(start)
            .filter(|frames| *frames > 0)
            .ok_or_else(|| "command law has nonpositive output extent".to_owned())?;
        let span = SourceSpan::approaching(position.0, position.1, law, 0, rate, frames)
            .ok_or_else(|| "declared command law cannot represent its source span".to_owned())?;
        position = span
            .source_ratio_at(frames)
            .ok_or_else(|| "declared command law has no source endpoint".to_owned())?;
        let span = native_input
            .map_output_span(input_origin, span)
            .ok_or_else(|| "command law cannot publish its decoder source clock".to_owned())?;
        expected.push((start, span));
    }
    check_commanded_spans(marks, issued, end, &expected)
}

fn check_commanded_spans(
    marks: &[(SessionFrame, SlotSnapshot)],
    issued: SessionFrame,
    end: SessionFrame,
    expected: &[(SessionFrame, SourceSpan)],
) -> Result<Vec<String>, String> {
    let mut checked = std::collections::BTreeSet::new();
    for (_, slot) in marks {
        let mark = slot
            .mark
            .ok_or_else(|| "command PCM lost its mark".to_owned())?;
        let info = slot
            .source
            .ok_or_else(|| "command PCM lost its source".to_owned())?;
        let span = info
            .source_span
            .ok_or_else(|| "command PCM lost its exact SourceSpan".to_owned())?;
        let start = i64::try_from(mark.lane.frame)
            .ok()
            .and_then(|lane| i64::from(mark.session).checked_sub(lane))
            .and_then(|frame| frame.checked_add(i64::try_from(info.lane_frame).ok()?))
            .map(SessionFrame::new)
            .ok_or_else(|| "command source span has no session interval".to_owned())?;
        if !checked.insert((start, info.render_revision)) {
            continue;
        }
        for offset in 0..=span.output_frames() {
            let frame = frame_after(start, offset)?;
            if frame < issued || frame > end {
                continue;
            }
            let index = expected
                .partition_point(|(start, _)| {
                    *start == issued
                        || *start < frame
                        || *start == frame && (offset == 0 || offset < span.output_frames())
                })
                .checked_sub(1)
                .ok_or_else(|| "command expected law does not cover actual PCM".to_owned())?;
            let (origin, law) = expected[index];
            let expected = law
                .source_ratio_at(
                    frame
                        .frames_since(origin)
                        .ok_or_else(|| "expected source span reverses".to_owned())?,
                )
                .ok_or_else(|| "expected source law has a coverage gap".to_owned())?;
            let actual = span
                .source_ratio_at(offset)
                .ok_or_else(|| "actual source span has an unmapped output frame".to_owned())?;
            if actual != expected {
                return Ok(vec![format!(
                    "consumed SourceSpan at {frame:?} maps {actual:?}, commanded Host/grid law requires {expected:?}"
                )]);
            }
        }
    }
    Ok(Vec::new())
}

pub(super) fn executed_law(
    model: &BeatGridModel,
    host_grid: &BeatGridSnapshot,
    observation: &LinkObservation,
    snapshot: &kithara::link::LinkedSnapshot<kithara::play::TrackSnapshot>,
    capture_start: SlotSnapshot,
    consumed: SlotSnapshot,
) -> Result<LawProof, String> {
    let rate = observation
        .consumed_rate
        .ok_or_else(|| "law has no consumed evaluation clock".to_owned())?;
    let item = snapshot.track.item;
    let status = snapshot.sync;
    let configured_speed = snapshot.track.speed;
    let (mark, media) = consumed_media(consumed)?;
    if snapshot.track.mark != Some(mark) {
        return Err("published source snapshot does not read the consumed PCM mark".to_owned());
    }
    let (start_mark, _) = consumed_media(capture_start)?;
    if start_mark.lane.segment != mark.lane.segment
        || capture_start
            .source
            .map(|info| (info.source_revision, info.media_origin))
            != consumed
                .source
                .map(|info| (info.source_revision, info.media_origin))
    {
        return Err("capture changed its consumed Segment or source origin".to_owned());
    }
    if mark.lane.frame.checked_sub(start_mark.lane.frame)
        != mark.session.frames_since(start_mark.session)
    {
        return Err("capture's consumed lane/session frames do not advance together".to_owned());
    }
    let curves = applied_curves(observation, item, mark, rate)?;
    let projected = projected_media(
        observation,
        item,
        &curves,
        capture_start,
        mark.session,
        rate,
    )?;
    let mut failures = configured_speed_failures(
        observation,
        item,
        mark.lane.segment,
        &curves,
        configured_speed,
    )?;
    let (host_history_input, _) = owned_host_history(observation, rate)?;
    let mut reference =
        consumed_cue_law(model, observation, &curves, snapshot, rate, &mut failures)?;
    let source_grid = declared_source_grid(model, rate)?;
    let source_beat = grid_beat_at(&source_grid, media_position(media, rate)?)?;
    let projected_beat = grid_beat_at(&source_grid, media_position(projected, rate)?)?;
    let host_beat = grid_beat_at(host_grid, MapPosition::Session(mark.session))?;
    let expected_consumed = session_frame_at(host_grid, host_beat + projected_beat - source_beat)?;
    if expected_consumed != mark.session {
        failures.push(format!("Applied source-law predicts {projected:?} at {:?}, actual PCM mark is {media:?}, bound frame {:?}", mark.session, expected_consumed));
    }
    let claimed_end = match status {
        kithara::link::SyncStatus::On => None,
        kithara::link::SyncStatus::Correcting { remaining } => Some(mark.session + remaining),
        other => {
            return Err(format!(
                "playing SYNC has no executed phase contract: {other:?}"
            ));
        }
    };
    let active = curves.partition_point(|curve| curve.at <= mark.session);
    let latest = active.checked_sub(1);
    let mut verified = Vec::new();
    let mut group_start = 0;
    for group in curves[..active]
        .chunk_by(|first, second| first.command.before.duration == second.command.before.duration)
    {
        let geometry = SourceGeometry::new(model, rate, group[0].command.before.duration)?;
        for (index, applied) in group.iter().enumerate() {
            let (evidence, anchor) = applied.verify(
                &geometry,
                observation,
                &curves,
                group_start + index,
                snapshot,
                &mut failures,
            )?;
            verified.push(evidence);
            reference = Some(anchor);
        }
        group_start += group.len();
    }
    let speed = if let Some(index) = latest {
        let applied = &curves[index];
        curve_rate(
            &applied.command.curve,
            mark.session
                .frames_since(applied.at)
                .ok_or_else(|| "consumed law precedes Applied".to_owned())?,
        )?
    } else {
        if claimed_end.is_some() {
            return Err("Correcting has no actually applied source law".to_owned());
        }
        let cue = owned_cue(observation, item, mark.lane.segment)?;
        curve_rate(&cue.curve, mark.lane.frame)?
    };
    Ok((
        speed,
        failures,
        serde_json::json!({"host_history_input": host_history_input, "verified_applied": verified, "capture_start": format!("{start_mark:?}"), "capture_end": format!("{mark:?}"), "correction_end": claimed_end.map(i64::from), "projected_media_ns": projected.as_nanos().to_string(), "supplemental_source_support": "supported"}),
        Some(reference.ok_or_else(|| {
            "consumed law has no independently declared source anchor".to_owned()
        })?),
    ))
}

fn source_regions(grid: &BeatGridSnapshot) -> Result<Vec<(MapPosition, MapPosition)>, String> {
    let mut cursor = media_seconds_position(0.0, NonZeroU32::MIN)?;
    let mut regions = Vec::new();
    loop {
        let BeatGridQuery::Resolved(kithara::warp::BeatGridRegion::Bounded(region)) =
            grid.region_at(MapPoint::new(grid.stamp(), cursor))
        else {
            return Err("supplied grid has no complete bounded region traversal".to_owned());
        };
        if regions.last().is_some_and(|(_, end)| *end == region.end()) {
            break;
        }
        regions.push((region.start(), region.end()));
        cursor = region.end();
    }
    Ok(regions)
}

fn queue_source_supported(
    model: &BeatGridModel,
    rate: NonZeroU32,
    consumed: SlotSnapshot,
) -> Result<bool, String> {
    let (mark, _) = consumed_media(consumed)?;
    let info = consumed
        .source
        .filter(|info| info.segment == mark.lane.segment)
        .ok_or_else(|| "Queue mark has no current source identity".to_owned())?;
    let span = info
        .source_span
        .ok_or_else(|| "Queue mark has no exact source span".to_owned())?;
    let offset = mark
        .lane
        .frame
        .checked_sub(info.lane_frame)
        .ok_or_else(|| "Queue mark precedes its source span".to_owned())?;
    if span.position_at(offset) != Some(mark.position) {
        return Err("Queue mark differs from its exact source span".to_owned());
    }
    let media = consumed_media_seconds(
        span,
        offset,
        info.media_origin
            .ok_or_else(|| "Queue source has no immutable media origin".to_owned())?,
    )
    .ok_or_else(|| "Queue exact source coordinate cannot translate to media".to_owned())?;
    let grid = declared_source_grid(model, rate)?;
    let point = media_seconds_position(media, rate)?;
    match grid.beat_at(MapPoint::new(grid.stamp(), point)) {
        BeatGridQuery::Resolved(_) => Ok(true),
        BeatGridQuery::Uncovered { .. } => {
            let regions = source_regions(&grid)?;
            let MapPosition::Asset(last) = regions
                .last()
                .ok_or_else(|| "Queue supplied grid has no regions".to_owned())?
                .1
            else {
                return Err("Queue supplied grid has a different source axis".to_owned());
            };
            let end = model
                .as_raw()
                .duration
                .ok_or_else(|| "uncovered Queue position has no finite asset extent".to_owned())?;
            if media * f64::from(rate.get()) > f64::from(last) && media <= end {
                Ok(false)
            } else {
                Err(format!(
                    "Queue source coordinate {point:?} is outside its finite tail"
                ))
            }
        }
        other => Err(format!("Queue supplied-grid query failed: {other:?}")),
    }
}

pub(super) fn queue_observation(
    model: &BeatGridModel,
    host_grid: &BeatGridSnapshot,
    observation: &LinkObservation,
    snapshot: &kithara::link::LinkedSnapshot<kithara::play::TrackSnapshot>,
    capture_start: SlotSnapshot,
    consumed: SlotSnapshot,
) -> Result<LawProof, String> {
    let rate = observation
        .consumed_rate
        .ok_or_else(|| "Queue observation has no native clock".to_owned())?;
    if queue_source_supported(model, rate, consumed)? {
        return executed_law(
            model,
            host_grid,
            observation,
            snapshot,
            capture_start,
            consumed,
        );
    }
    let (mark, _) = consumed_media(consumed)?;
    if snapshot.track.mark != Some(mark) {
        return Err("Queue tail snapshot differs from its consumed PCM mark".to_owned());
    }
    let (first_mark, _) = consumed_media(capture_start)?;
    if first_mark.lane.segment != mark.lane.segment
        || mark.lane.frame.checked_sub(first_mark.lane.frame)
            != mark.session.frames_since(first_mark.session)
        || capture_start
            .source
            .map(|info| (info.source_revision, info.media_origin))
            != consumed
                .source
                .map(|info| (info.source_revision, info.media_origin))
    {
        return Err("Queue tail lost its original source/lane/session custody".to_owned());
    }
    let correction_end = match snapshot.sync {
        kithara::link::SyncStatus::On => None,
        kithara::link::SyncStatus::Correcting { remaining } => {
            Some(i64::from(mark.session + remaining))
        }
        other => {
            return Err(format!(
                "Queue tail has no playing SYNC contract: {other:?}"
            ));
        }
    };
    let curves = applied_curves(observation, snapshot.track.item, mark, rate)?;
    let failures = configured_speed_failures(
        observation,
        snapshot.track.item,
        mark.lane.segment,
        &curves,
        snapshot.track.speed,
    )?;
    let speed = if let Some(applied) = curves.iter().rev().find(|curve| curve.at <= mark.session) {
        curve_rate(
            &applied.command.curve,
            mark.session
                .frames_since(applied.at)
                .ok_or_else(|| "Queue tail precedes its applied law".to_owned())?,
        )?
    } else {
        curve_rate(
            &owned_cue(observation, snapshot.track.item, mark.lane.segment)?.curve,
            mark.lane.frame,
        )?
    };
    Ok((
        speed,
        failures,
        serde_json::json!({
            "correction_end": correction_end,
            "supplemental_source_support": "uncovered_finite_tail",
            "primary_forward_check": "required_after_capture",
            "capture_end": format!("{mark:?}"),
        }),
        None,
    ))
}

#[derive(Debug)]
pub(super) struct QueueLaw {
    anchor: SelectedBinding,
    spans: Vec<(SessionFrame, SourceSpan)>,
    curves: Vec<(SessionFrame, SpeedCurve)>,
    deadlines: Vec<(SessionFrame, SessionFrame)>,
    axes: (f64, f64, f64),
    host: TempoTrajectory,
    source: BeatGridSnapshot,
    start: SessionFrame,
    end: SessionFrame,
}

impl QueueLaw {
    fn correction(
        &self,
        observation: &LinkObservation,
        applied: &AppliedCurve<'_>,
        grid: &SourceGrid,
        media_origin: Duration,
    ) -> Result<(SpeedCurve, u64), String> {
        let command = applied.command;
        if command.landing.is_some() {
            return Err("Queue source law unexpectedly relocates after native Start".to_owned());
        }
        let input = command
            .input
            .and_then(|index| observation.owner_inputs.get(index))
            .ok_or_else(|| "Queue applied law has no typed public operation input".to_owned())?;
        let commanded = match input.kind {
            LinkOwnerInputKind::Retime => true,
            LinkOwnerInputKind::Realign => false,
            _ => return Err("Queue applied correction has an unexpected public cause".to_owned()),
        };
        if input.at != Some(applied.at) {
            return Err("Queue correction Applied frame differs from its public cause".to_owned());
        }
        let host = applied.owner_host(observation)?;
        let before = command
            .before
            .mark
            .ok_or_else(|| "Queue correction has no pre-command snapshot time".to_owned())?;
        let planned = self.planned(before, applied.at, media_origin)?;
        if &planned != applied.planned_input()? {
            return Err("Queue planned(C) differs from independent prior source law".to_owned());
        }
        let (curve, deadline) = corrected_speed_curve(
            host,
            grid,
            planned.0,
            applied.at,
            planned.1,
            LinkConfig::default().epsilon(),
            commanded,
        )
        .ok_or_else(|| "Queue public correction has no canonical finite law".to_owned())?;
        if curve != command.curve {
            return Err("Queue emitted correction differs from public Host/cue intent".to_owned());
        }
        Ok((curve, deadline))
    }

    fn span_at(&self, frame: SessionFrame) -> Result<(SessionFrame, SourceSpan), String> {
        let index = self
            .spans
            .partition_point(|(at, _)| *at <= frame)
            .checked_sub(1)
            .ok_or_else(|| "Queue expected law precedes its native Start".to_owned())?;
        let (at, span) = self.spans[index];
        let offset = frame
            .frames_since(at)
            .ok_or_else(|| "Queue expected source axis reverses".to_owned())?;
        if offset > span.output_frames() {
            return Err("Queue expected source law has a coverage gap".to_owned());
        }
        Ok((at, span))
    }

    fn ratio_at(&self, frame: SessionFrame) -> Result<(u128, NonZeroU128), String> {
        let (at, span) = self.span_at(frame)?;
        span.source_ratio_at(
            frame
                .frames_since(at)
                .ok_or_else(|| "Queue expected source axis reverses".to_owned())?,
        )
        .ok_or_else(|| "Queue expected source law has no exact coordinate".to_owned())
    }

    fn position_at(&self, frame: SessionFrame) -> Result<Duration, String> {
        let (at, span) = self.span_at(frame)?;
        span.position_at(
            frame
                .frames_since(at)
                .ok_or_else(|| "Queue expected source axis reverses".to_owned())?,
        )
        .ok_or_else(|| "Queue expected source law has no media coordinate".to_owned())
    }

    fn planned(
        &self,
        before: SlotMark,
        at: SessionFrame,
        origin: Duration,
    ) -> Result<(Duration, f64), String> {
        if self.position_at(before.session)? != before.position {
            return Err("Queue pre-command mark differs from independent expected PCM".to_owned());
        }
        let mut position = self.position_at(before.session)?.as_secs_f64();
        let mut cursor = before.session;
        for (index, (start, curve)) in self.curves.iter().enumerate() {
            let stop = self
                .curves
                .get(index + 1)
                .map_or(at, |(next, _)| (*next).min(at));
            if stop <= cursor || *start > at {
                continue;
            }
            position += curve_advance(
                curve,
                cursor
                    .frames_since(*start)
                    .ok_or_else(|| "Queue projection has no predecessor law".to_owned())?,
                stop.frames_since(*start)
                    .ok_or_else(|| "Queue projection reverses".to_owned())?,
            )? / f64::from(self.host.sample_rate().get());
            cursor = stop;
            if cursor == at {
                break;
            }
        }
        if cursor != at {
            return Err("Queue projection does not reach its command frame".to_owned());
        }
        let (start, curve) = self
            .curves
            .iter()
            .rev()
            .find(|(start, _)| *start <= at)
            .ok_or_else(|| "Queue projection has no active expected law".to_owned())?;
        let position = position - origin.as_secs_f64();
        if position < 0.0 {
            return Err("Queue projected media precedes its immutable origin".to_owned());
        }
        Ok((
            Duration::try_from_secs_f64(position).map_err(|error| error.to_string())?,
            curve_rate(
                curve,
                at.frames_since(*start)
                    .ok_or_else(|| "Queue predecessor law reverses".to_owned())?,
            )?,
        ))
    }

    pub(super) fn check_status(
        &self,
        frame: SessionFrame,
        claimed: Option<i64>,
    ) -> Result<(), String> {
        let expected = self
            .deadlines
            .iter()
            .rev()
            .find(|(at, _)| *at <= frame)
            .and_then(|(_, end)| (frame < *end).then_some(i64::from(*end)));
        if expected != claimed {
            return Err(format!(
                "Queue frame {frame:?}: published correction end {claimed:?}, public intent requires {expected:?}"
            ));
        }
        Ok(())
    }

    pub(super) fn check_spans(
        &self,
        actual: &[(i64, SourceSpan)],
    ) -> Result<serde_json::Value, String> {
        let mut frontier = i64::from(self.start);
        let mut frames = 0_u64;
        for &(start, span) in actual {
            let stop = start
                .checked_add(
                    i64::try_from(span.output_frames()).map_err(|error| error.to_string())?,
                )
                .ok_or_else(|| "Queue consumed span overflows".to_owned())?;
            let from = start.max(i64::from(self.start));
            let to = stop.min(i64::from(self.end));
            if from != frontier || to <= from {
                return Err(format!(
                    "Queue forward ledger loses consumed coverage at {frontier}"
                ));
            }
            let mut cursor = from;
            while cursor < to {
                let index = self
                    .spans
                    .partition_point(|(at, _)| i64::from(*at) <= cursor)
                    .checked_sub(1)
                    .ok_or_else(|| "Queue consumed span precedes its expected law".to_owned())?;
                let (origin, law) = self.spans[index];
                let end = self
                    .spans
                    .get(index + 1)
                    .map_or(to, |(at, _)| to.min(i64::from(*at)));
                let expected = law
                    .for_output_range(
                        u64::try_from(cursor - i64::from(origin))
                            .map_err(|error| error.to_string())?
                            ..u64::try_from(end - i64::from(origin))
                                .map_err(|error| error.to_string())?,
                    )
                    .ok_or_else(|| "Queue expected law loses consumed coverage".to_owned())?;
                let observed = span
                    .for_output_range(
                        u64::try_from(cursor - start).map_err(|error| error.to_string())?
                            ..u64::try_from(end - start).map_err(|error| error.to_string())?,
                    )
                    .ok_or_else(|| "Queue consumed span loses its exact mapping".to_owned())?
                    .with_mapping_revision(None)
                    .with_render_revision(0);
                // Exact analytic bases prove every coordinate, including slice endpoints.
                if observed != expected {
                    return Err(format!(
                        "Queue forward law over frames {cursor}..{end}: consumed {observed:?}, expected {expected:?}"
                    ));
                }
                cursor = end;
            }
            frames += u64::try_from(to - from).map_err(|error| error.to_string())?;
            frontier = to;
        }
        if frontier != i64::from(self.end) || frames == 0 {
            return Err("Queue forward ledger does not reach the native end".to_owned());
        }
        let mut beats = std::collections::BTreeSet::new();
        for (start, end) in source_regions(&self.source)? {
            let first = grid_beat_at(&self.source, start)?
                .ceil()
                .to_i64()
                .ok_or_else(|| "Queue first supported beat is unrepresentable".to_owned())?;
            let last = grid_beat_at(&self.source, end)?
                .floor()
                .to_i64()
                .ok_or_else(|| "Queue final supported beat is unrepresentable".to_owned())?;
            beats.extend(first..=last);
        }
        let mut census = Vec::new();
        for beat in beats {
            let beat = beat
                .to_f64()
                .ok_or_else(|| "Queue supported beat is unrepresentable".to_owned())?;
            let source = source_media_for_beat(&self.source, beat, self.host.sample_rate())?;
            let host_beat = self.anchor.host_beat + (beat - self.anchor.source_beat);
            let at = self
                .host
                .frame_at(SessionBeat::new(host_beat).map_err(|error| error.to_string())?);
            let (origin, source_stride, host_stride) = self.axes;
            let phase = (beat - origin).rem_euclid(source_stride);
            let phase_beat = phase + ((host_beat - phase) / host_stride).round() * host_stride;
            if self
                .host
                .frame_at(SessionBeat::new(phase_beat).map_err(|error| error.to_string())?)
                != at
            {
                return Err(format!(
                    "Queue source beat {beat} and Host beat {host_beat} have incompatible declared meter phases"
                ));
            }
            let audible = at >= self.start && at < self.end;
            if audible {
                let index = actual
                    .partition_point(|(start, _)| *start <= i64::from(at))
                    .checked_sub(1)
                    .ok_or_else(|| "Queue supported beat has no consumed witness".to_owned())?;
                let (start, span) = actual[index];
                let actual = span
                    .source_ratio_at(
                        u64::try_from(i64::from(at) - start).map_err(|error| error.to_string())?,
                    )
                    .ok_or_else(|| "Queue supported beat lies in a consumed gap".to_owned())?;
                if actual != self.ratio_at(at)? {
                    return Err(format!(
                        "Queue supported source beat {beat} has the wrong exact native source coordinate"
                    ));
                }
            }
            census.push(
                serde_json::json!({"source_beat": beat, "source_media": source,
                "host_beat": host_beat, "native_frame": i64::from(at), "audible": audible,
                "terminal_endpoint": at == self.end}),
            );
        }
        if !census.iter().any(|entry| entry["audible"] == true) {
            return Err("Queue has no audible supported source-beat witness".to_owned());
        }
        Ok(
            serde_json::json!({"exact_forward_frames": frames, "supported_source_beats": census,
            "law_basis": "declared cue, native Start, public Host trajectory and correction intent"}),
        )
    }
}

#[kithara::test(native, timeout(Duration::from_secs(2)))]
fn queue_forward_law_checks_large_spans_across_analytic_boundaries() {
    let rate = NonZeroU32::new(48_000).expect("source rate");
    let frames = 1_000_000_000;
    let curve = ExponentialApproach::new(1.0, 1.0, 0.0).expect("constant source law");
    let span = SourceSpan::approaching(0, NonZeroU128::MIN, curve, 0, rate, frames)
        .expect("large exact source map");
    let ArtifactSource::Value(model) = synthetic_grid() else {
        panic!("declared source grid");
    };
    let law = QueueLaw {
        anchor: SelectedBinding {
            media: Duration::ZERO,
            source_beat: 0.0,
            host_beat: 0.0,
            frame: SessionFrame::new(0),
        },
        spans: vec![
            (SessionFrame::new(0), span.for_output_range(0..64).unwrap()),
            (
                SessionFrame::new(64),
                span.for_output_range(64..frames).unwrap(),
            ),
        ],
        curves: vec![],
        deadlines: vec![],
        axes: (0.0, 1.0, 1.0),
        host: TempoTrajectory::new(
            TempoStep {
                frame: SessionFrame::new(0),
                beat: SessionBeat::default(),
                tempo: Tempo::new(120.0).expect("Host tempo"),
            },
            NonZeroU16::new(4).expect("Host meter"),
            rate,
        ),
        source: source_with_duration(&model, rate, None).expect("bounded source grid"),
        start: SessionFrame::new(0),
        end: SessionFrame::new(frames.try_into().unwrap()),
    };
    let evidence = law.check_spans(&[(0, span)]).expect("complete exact map");
    assert_eq!(evidence["exact_forward_frames"], frames);
    let shifted = span
        .scaled_from(
            (0, NonZeroU128::MIN),
            (1, NonZeroU128::MIN),
            (1, NonZeroU128::MIN),
        )
        .expect("one-frame displaced source");
    assert!(law.check_spans(&[(0, shifted)]).is_err());
    assert!(law.check_spans(&[(1, span)]).is_err());
    assert!(
        law.check_spans(&[(0, span.for_output_range(0..frames - 1).unwrap())])
            .is_err()
    );
}

fn physical_phase(position: (u128, NonZeroU128)) -> Result<(u128, NonZeroU128), String> {
    let scale = 1u128 << 32;
    let scaled = position
        .0
        .checked_mul(scale)
        .ok_or_else(|| "physical phase overflows Q32".to_owned())?;
    let denominator = position.1.get();
    let remainder = scaled % denominator;
    Ok((
        scaled / denominator + u128::from(remainder >= denominator - remainder),
        NonZeroU128::new(scale).expect("Q32 source phase"),
    ))
}

fn append_queue_curve(
    spans: &mut Vec<(SessionFrame, SourceSpan)>,
    curve: &SpeedCurve,
    retained: std::ops::Range<SessionFrame>,
    mut position: (u128, NonZeroU128),
    input: SourceSpan,
    input_origin: u64,
    rate: NonZeroU32,
) -> Result<(), String> {
    let (start, end) = (retained.start, retained.end);
    let parts = analytic_parts(curve)?;
    for (index, &(offset, law)) in parts.iter().enumerate() {
        let at = frame_after(start, offset)?;
        if at >= end {
            break;
        }
        let stop = parts
            .get(index + 1)
            .map(|(next, _)| frame_after(start, *next))
            .transpose()?
            .unwrap_or(end)
            .min(end);
        let frames = stop
            .frames_since(at)
            .filter(|frames| *frames > 0)
            .ok_or_else(|| "Queue expected analytic part has no positive extent".to_owned())?;
        let span = SourceSpan::approaching(position.0, position.1, law, 0, rate, frames)
            .ok_or_else(|| "Queue expected analytic part is unrepresentable".to_owned())?;
        position = span
            .source_ratio_at(frames)
            .ok_or_else(|| "Queue expected part has no exact endpoint".to_owned())?;
        spans.push((
            at,
            input
                .map_output_span(input_origin, span)
                .ok_or_else(|| "Queue expected law cannot publish the decoder clock".to_owned())?,
        ));
    }
    Ok(())
}

fn sync_source_grid(
    model: &BeatGridModel,
    rate: NonZeroU32,
    duration: Option<Duration>,
) -> Result<SourceGrid, String> {
    let extent = duration
        .map(|duration| {
            AudioSpec::new(1, rate)
                .frames_for(duration)
                .map_err(|error| error.to_string())
                .and_then(|frames| u64::try_from(frames.get()).map_err(|error| error.to_string()))
        })
        .transpose()?;
    SourceGrid::new(
        model.geometry(),
        rate,
        extent,
        matches!(model.as_raw().state, BeatGridState::Final) && extent.is_some(),
    )
    .map_err(|error| error.to_string())
}

pub(super) fn queue_law(
    model: &BeatGridModel,
    observation: &LinkObservation,
    item: kithara::events::TrackId,
    retained: std::ops::Range<SessionFrame>,
    first: (SlotSnapshot, u64),
    declared_cue: Duration,
    decoded: &super::control::DecodedCapture,
) -> Result<QueueLaw, String> {
    let (start, end) = (retained.start, retained.end);
    let (first, ready_frame) = first;
    let (mark, _) = consumed_media(first)?;
    let info = first
        .source
        .ok_or_else(|| "Queue cue has no native source identity".to_owned())?;
    let rate = info.spec.sample_rate;
    let cue = owned_cue(observation, item, mark.lane.segment)?;
    if cue.position != declared_cue
        || mark.lane.frame
            != mark
                .session
                .frames_since(start)
                .ok_or_else(|| "Queue source precedes its native Start".to_owned())?
    {
        return Err("Queue cue or lane origin differs from its declared native entry".to_owned());
    }
    let host = cue
        .host_input
        .and_then(|index| observation.owner_inputs.get(index))
        .and_then(|input| input.trajectory.as_ref())
        .ok_or_else(|| "Queue cue has no public Host trajectory input".to_owned())?;
    let source = source_with_duration(model, rate, cue.before.duration)?;
    let source_beat = grid_beat_at(&source, media_position(declared_cue, rate)?)?;
    let (origin, source_stride, host_stride) = phase_axes(model, 4)?;
    let phase = (source_beat - origin).rem_euclid(source_stride);
    let host_beat =
        phase + ((f64::from(host.beat_at(start)) - phase) / host_stride).round() * host_stride;
    if host.frame_at(SessionBeat::new(host_beat).map_err(|error| error.to_string())?) != start {
        return Err("Queue native Start does not bind the declared selected cue".to_owned());
    }
    let anchor = SelectedBinding {
        media: declared_cue,
        source_beat,
        host_beat,
        frame: start,
    };
    let grid = sync_source_grid(model, rate, cue.before.duration)?;
    let curve = speed_curve(host, &grid, declared_cue, start, None)
        .ok_or_else(|| "Queue declared cue has no canonical source law".to_owned())?;
    if curve != cue.curve {
        return Err("Queue emitted cue law differs from declared cue/Host intent".to_owned());
    }
    let media_origin = info
        .media_origin
        .ok_or_else(|| "Queue source has no immutable media origin".to_owned())?;
    let raw = media_origin
        .checked_add(declared_cue)
        .ok_or_else(|| "Queue declared raw cue overflows".to_owned())?;
    let initial = (
        raw.as_nanos()
            .checked_mul(u128::from(rate.get()))
            .ok_or_else(|| "Queue declared raw cue overflows its source clock".to_owned())?,
        NonZeroU128::new(1_000_000_000).expect("nanosecond source clock"),
    );
    let (input_origin, decoder_input) =
        decoded
            .physical_frame_basis(info, ready_frame)
            .map_err(|error| {
                format!(
                    "{error}; first_consumed={first:?}; native_ready_frame={ready_frame}; \
             declared_cue={declared_cue:?}"
                )
            })?;
    let initial = if decoder_input.position_at(0) == Some(raw) {
        decoder_input
            .source_ratio_at(0)
            .ok_or_else(|| "Queue decoder basis has no exact origin".to_owned())?
    } else {
        initial
    };
    let initial = decoder_input
        .output_ratio_at_source(input_origin, initial)
        .ok_or_else(|| "Queue cue cannot translate through its decoder clock".to_owned())?;
    let native_input = decoder_input
        .scaled_from(
            (0, NonZeroU128::MIN),
            (0, NonZeroU128::MIN),
            (
                u128::from(decoder_input.sample_rate().get()),
                NonZeroU128::from(rate),
            ),
        )
        .ok_or_else(|| "Queue decoder source units cannot be restored".to_owned())?;
    let mut result = QueueLaw {
        anchor,
        spans: Vec::new(),
        curves: vec![(start, curve)],
        deadlines: Vec::new(),
        axes: phase_axes(model, 4)?,
        host: host.clone(),
        source,
        start,
        end,
    };
    append_queue_curve(
        &mut result.spans,
        &result.curves[0].1,
        start..end,
        initial,
        native_input,
        input_origin,
        rate,
    )?;
    for applied in applied_curves(observation, item, mark, rate)? {
        if applied.at < start {
            return Err("Queue applied law precedes native Start".to_owned());
        }
        if applied.at >= end {
            continue;
        }
        let (curve, deadline) = result.correction(observation, &applied, &grid, media_origin)?;
        let host = applied.owner_host(observation)?;
        let mut position = decoder_input
            .output_ratio_at_source(input_origin, result.ratio_at(applied.at)?)
            .ok_or_else(|| "Queue replacement cannot translate its decoder clock".to_owned())?;
        position = physical_phase(position)?;
        result.spans.retain(|(at, _)| *at < applied.at);
        if let Some((at, span)) = result.spans.last_mut() {
            let frames = applied
                .at
                .frames_since(*at)
                .ok_or_else(|| "Queue replacement reverses".to_owned())?;
            *span = span
                .for_output_range(0..frames)
                .ok_or_else(|| "Queue predecessor cannot retain its analytic basis".to_owned())?;
        }
        append_queue_curve(
            &mut result.spans,
            &curve,
            applied.at..end,
            position,
            native_input,
            input_origin,
            rate,
        )?;
        result.curves.push((applied.at, curve));
        result
            .deadlines
            .push((applied.at, frame_after(applied.at, deadline)?));
        result.host = host.clone();
    }
    result.host = observation
        .owner_inputs
        .iter()
        .rev()
        .find_map(|input| input.trajectory.as_ref())
        .cloned()
        .ok_or_else(|| "Queue has no complete public Host history".to_owned())?;
    Ok(result)
}

#[derive(Clone, Copy, Debug)]
pub(super) struct SelectedBinding {
    pub(super) media: Duration,
    pub(super) source_beat: f64,
    pub(super) host_beat: f64,
    pub(super) frame: SessionFrame,
}

pub(super) fn sync_precedes_seek(order: OperationOrder) -> bool {
    order
        .operations()
        .iter()
        .take_while(|operation| !matches!(operation, Operation::Seek))
        .any(|operation| matches!(operation, Operation::Sync))
}

pub(super) fn relocation_count(
    observation: &LinkObservation,
    item: kithara::events::TrackId,
    model: &BeatGridModel,
    case: SyncCase,
    required: Option<usize>,
) -> Result<Option<usize>, String> {
    if required.is_some() || case.paused {
        return Ok(required);
    }
    if !sync_precedes_seek(case.order) {
        return Ok(Some(1));
    }
    let request = observation
        .sync_requests
        .iter()
        .find(|request| request.on)
        .ok_or_else(|| "no actual initial Sync request".to_owned())?;
    let input = observation
        .owner_inputs
        .get(request.input)
        .filter(|input| matches!(input.kind, LinkOwnerInputKind::Sync { on: true }))
        .ok_or_else(|| "initial Sync has no owned operation input".to_owned())?;
    let track = request
        .tracks
        .iter()
        .find(|track| track.track.item == item)
        .ok_or_else(|| "initial Sync has no actual per-item outcome snapshot".to_owned())?;
    let admitted = observation
        .placements
        .get(request.placements.clone())
        .ok_or_else(|| "initial Sync has no valid admission range".to_owned())?;
    let first = observation
        .placements
        .iter()
        .find(|placement| placement.item == item)
        .ok_or_else(|| "no admitted relocation follows the Sync request".to_owned())?;
    if let Some(seq) = request.seq {
        if first.seq != seq
            || first.input != Some(request.input)
            || !admitted
                .iter()
                .any(|placement| placement.item == item && placement.seq == seq)
            || input.at != Some(first.now)
        {
            return Err("initial Sync Seq does not own the first admitted relocation".to_owned());
        }
        return Ok(Some(2));
    }
    if admitted.iter().any(|placement| placement.item == item) {
        return Err("initial Sync returned None after admitting a relocation".to_owned());
    }
    let kithara::link::SyncStatus::WaitingForGrid { required } = track.sync else {
        return Err("initial Sync None has no actual WaitingForGrid outcome".to_owned());
    };
    let served = observation.owner_inputs[..request.input]
        .iter()
        .rev()
        .find_map(|input| match &input.kind {
            LinkOwnerInputKind::Grid { answer } if answer.item == item => Some(answer),
            _ => None,
        })
        .ok_or_else(|| "initial Sync waiting has no actually served grid".to_owned())?;
    let load = observation
        .loads
        .iter()
        .rev()
        .find_map(|(owner, seq)| (*owner == item).then_some(*seq));
    if load != Some(served.load) || served.model.as_ref() != Ok(model) {
        return Err(
            "initial Sync waiting grid has no current Load or declared-model custody".to_owned(),
        );
    }
    let raw = model.as_raw();
    let (Some(first_beat), Some(last_beat)) = (raw.beats.first(), raw.beats.last()) else {
        return Err("initial Sync waiting grid has no observed beat span".to_owned());
    };
    let start = Duration::try_from_secs_f64(first_beat.at).map_err(|error| error.to_string())?;
    let end = Duration::try_from_secs_f64(last_beat.at).map_err(|error| error.to_string())?;
    if (start..=end).contains(&required) {
        return Err("initial Sync WaitingForGrid required position is covered".to_owned());
    }
    if first.input != Some(request.input) || input.at.is_none_or(|at| first.now < at) {
        return Err(
            "pending Sync relocation has a different operation or precedes its request".to_owned(),
        );
    }
    Ok(Some(1))
}

pub(super) fn relocation_request(
    order: OperationOrder,
    last: bool,
    requested: Duration,
    projected: Result<Duration, String>,
) -> Result<Duration, String> {
    if last && sync_precedes_seek(order) {
        Ok(requested)
    } else {
        projected
    }
}

/// Selects the nearest declared source position with the future Host phase.
pub(super) fn select_nearest(
    model: &BeatGridModel,
    evaluation_rate: NonZeroU32,
    host_grid: &BeatGridSnapshot,
    requested: Duration,
    frame: SessionFrame,
    host_beats_per_bar: u16,
) -> Result<SelectedBinding, String> {
    let (origin, source_stride, host_stride) = phase_axes(model, host_beats_per_bar)?;
    let source_grid = declared_source_grid(model, evaluation_rate)?;
    let requested_position = media_position(requested, evaluation_rate)?;
    let requested_beat = grid_beat_at(&source_grid, requested_position)?;
    let host_beat = grid_beat_at(host_grid, MapPosition::Session(frame))?;
    let host_phase = host_beat.rem_euclid(host_stride);
    if host_phase >= source_stride {
        return Err("future Host phase has no matching declared source phase".to_owned());
    }
    let phase = host_phase + origin;
    let earlier = phase + ((requested_beat - phase) / source_stride).floor() * source_stride;
    let candidates = [earlier, earlier + source_stride]
        .into_iter()
        .map(|beat| {
            source_media_at(&source_grid, evaluation_rate, beat)
                .map(|media| media.map(|media| (beat, media)))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let (source_beat, media) = candidates
        .into_iter()
        .flatten()
        .min_by_key(|(_, media)| requested.abs_diff(*media))
        .ok_or_else(|| {
            "no nonnegative in-phase source position lies in the declared grid".to_owned()
        })?;
    Ok(SelectedBinding {
        media,
        source_beat,
        host_beat,
        frame,
    })
}

/// Binds the first declared strong beat at or after cue to the earliest Host entry.
pub(super) fn select_entry(
    model: &BeatGridModel,
    evaluation_rate: NonZeroU32,
    host_grid: &BeatGridSnapshot,
    cue: Duration,
    bound: SessionFrame,
    host_beats_per_bar: u16,
) -> Result<SelectedBinding, String> {
    let (origin, source_stride, host_stride) = phase_axes(model, host_beats_per_bar)?;
    let raw = model.as_raw();
    let at = if raw.meter.is_some() {
        raw.downbeats
            .iter()
            .map(|beat| beat.at)
            .find(|at| *at >= cue.as_secs_f64())
    } else {
        raw.beats
            .iter()
            .map(|beat| beat.at)
            .find(|at| *at >= cue.as_secs_f64())
    }
    .ok_or_else(|| "cue has no declared following strong beat".to_owned())?;
    let media = Duration::try_from_secs_f64(at).map_err(|error| error.to_string())?;
    let source_grid = declared_source_grid(model, evaluation_rate)?;
    let source_beat = grid_beat_at(&source_grid, media_position(media, evaluation_rate)?)?;
    let bound_beat = grid_beat_at(host_grid, MapPosition::Session(bound))?;
    let phase = (source_beat - origin).rem_euclid(source_stride);
    let earlier = phase + ((bound_beat - phase) / host_stride).floor() * host_stride;
    let candidates = [earlier, earlier + host_stride]
        .into_iter()
        .map(|beat| session_frame_at(host_grid, beat).map(|frame| (beat, frame)))
        .collect::<Result<Vec<_>, _>>()?;
    let (host_beat, frame) = candidates
        .into_iter()
        .filter(|(_, frame)| *frame >= bound)
        .min_by_key(|(_, frame)| *frame)
        .ok_or_else(|| "declared Host grid has no entry at or after the bound".to_owned())?;
    Ok(SelectedBinding {
        media,
        source_beat,
        host_beat,
        frame,
    })
}

impl SelectedBinding {
    pub(super) fn command_failures(
        &self,
        model: &BeatGridModel,
        evaluation_rate: NonZeroU32,
        host_grid: &BeatGridSnapshot,
        actual_media: Duration,
        actual_frame: SessionFrame,
    ) -> Result<Vec<String>, String> {
        let expected_frame =
            self.frame_for_media(model, evaluation_rate, host_grid, actual_media)?;
        let mut failures = Vec::new();
        if expected_frame != self.frame {
            failures.push(format!("selected source beat {:.12} at {:?} was replaced by command position {:?}, which binds to Host frame {:?} instead of {:?}", self.source_beat, self.media, actual_media, expected_frame, self.frame));
        }
        if actual_frame != self.frame {
            failures.push(format!(
                "selected command belongs to Host frame {:?}, admitted at {:?}",
                self.frame, actual_frame
            ));
        }
        Ok(failures)
    }

    pub(super) fn consumption_failures(
        &self,
        model: &BeatGridModel,
        evaluation_rate: NonZeroU32,
        host_grid: &BeatGridSnapshot,
        consumed: SlotSnapshot,
        segment: SegmentId,
    ) -> Result<Vec<String>, String> {
        if consumed.state != SlotState::Playing {
            return Err(format!(
                "selected binding has no consumed playing slot: {:?}",
                consumed.state
            ));
        }
        let mark = consumed
            .mark
            .ok_or_else(|| "selected binding has no consumed PCM mark".to_owned())?;
        let media_origin = consumed
            .source
            .filter(|info| info.segment == mark.lane.segment)
            .and_then(|info| info.media_origin)
            .ok_or_else(|| "selected binding has no known media origin".to_owned())?;
        let media = mark
            .position
            .checked_sub(media_origin)
            .ok_or_else(|| "consumed raw position is before media zero".to_owned())?;
        let expected_frame = self.frame_for_media(model, evaluation_rate, host_grid, media)?;
        let mut failures = Vec::new();
        if expected_frame != mark.session {
            failures.push(format!("consumed media position {:?} binds to Host frame {:?} from selected source/Host beats {:.12}/{:.12}, consumed at {:?}", media, expected_frame, self.source_beat, self.host_beat, mark.session));
        }
        if mark.lane.segment != segment {
            failures.push(format!(
                "selected segment {:?} was replaced by consumed segment {:?}",
                segment, mark.lane.segment
            ));
        }
        Ok(failures)
    }

    pub(super) fn corrected_consumption_failures(
        &self,
        model: &BeatGridModel,
        host_grid: &BeatGridSnapshot,
        consumed: SlotSnapshot,
        segment: SegmentId,
        observation: &LinkObservation,
        item: kithara::events::TrackId,
    ) -> Result<Vec<String>, String> {
        let evaluation_rate = observation
            .consumed_rate
            .ok_or_else(|| "selected correction has no consumed evaluation clock".to_owned())?;
        let (mark, media) = consumed_media(consumed)?;
        if mark.lane.segment != segment {
            return Ok(vec![format!(
                "selected segment {:?} was replaced by consumed segment {:?}",
                segment, mark.lane.segment
            )]);
        }
        let origin = consumed
            .source
            .filter(|info| info.segment == mark.lane.segment)
            .and_then(|info| info.media_origin)
            .ok_or_else(|| "selected correction has no known media origin".to_owned())?;
        let delta = mark
            .session
            .frames_since(self.frame)
            .ok_or_else(|| "selected anchor is after the consumed mark".to_owned())?;
        let lane = LaneFrame {
            segment,
            frame: mark.lane.frame.checked_sub(delta).ok_or_else(|| {
                "selected anchor does not fit the consumed lane/session translation".to_owned()
            })?,
        };
        let mut anchor = consumed;
        anchor.mark = Some(SlotMark {
            lane,
            session: self.frame,
            position: origin + self.media,
        });
        let curves = applied_curves(observation, item, mark, evaluation_rate)?;
        let predicted = projected_media(
            observation,
            item,
            &curves,
            anchor,
            mark.session,
            evaluation_rate,
        )?;
        let actual_frame = self.frame_for_media(model, evaluation_rate, host_grid, media)?;
        let predicted_frame = self.frame_for_media(model, evaluation_rate, host_grid, predicted)?;
        if actual_frame != predicted_frame {
            return Ok(vec![format!(
                "selected source/Host beats {:.12}/{:.12} with actual Applied replacements predict media {:?} at {:?}, consumed {:?} binds to {:?}",
                self.source_beat, self.host_beat, predicted, predicted_frame, media, actual_frame
            )]);
        }
        Ok(Vec::new())
    }

    fn frame_for_media(
        &self,
        model: &BeatGridModel,
        evaluation_rate: NonZeroU32,
        host_grid: &BeatGridSnapshot,
        media: Duration,
    ) -> Result<SessionFrame, String> {
        let source_grid = declared_source_grid(model, evaluation_rate)?;
        let source_beat = grid_beat_at(&source_grid, media_position(media, evaluation_rate)?)?;
        let host_beat = self.host_beat + (source_beat - self.source_beat);
        session_frame_at(host_grid, host_beat)
    }
}

fn phase_axes(model: &BeatGridModel, host_beats_per_bar: u16) -> Result<(f64, f64, f64), String> {
    let host_stride = NonZeroU16::new(host_beats_per_bar)
        .ok_or_else(|| "Host meter must be non-zero".to_owned())?;
    Ok(model.as_raw().meter.map_or((0.0, 1.0, 1.0), |meter| {
        (
            meter.origin_beat_ordinal.as_(),
            f64::from(meter.beats_per_bar.get()),
            f64::from(host_stride.get()),
        )
    }))
}

fn declared_source_grid(
    model: &BeatGridModel,
    evaluation_rate: NonZeroU32,
) -> Result<BeatGridSnapshot, String> {
    BeatGridSnapshot::model(
        BeatGridId::allocate().map_err(|error| error.to_string())?,
        BeatGridRevision::first(),
        model,
        AssetAxis::new(evaluation_rate, AssetExtent::Unknown),
    )
    .map_err(|error| error.to_string())
}

fn media_position(media: Duration, evaluation_rate: NonZeroU32) -> Result<MapPosition, String> {
    AssetFrame::new(media.as_secs_f64() * f64::from(evaluation_rate.get()))
        .map(MapPosition::Asset)
        .map_err(|error| error.to_string())
}

fn grid_beat_at(grid: &BeatGridSnapshot, position: MapPosition) -> Result<f64, String> {
    let point = MapPoint::new(grid.stamp(), position);
    let BeatGridQuery::Resolved(beat) = grid.beat_at(point) else {
        return Err(format!("position {:?} has no declared grid beat", position));
    };
    Ok(f64::from(*beat.value().value()))
}

fn session_frame_at(grid: &BeatGridSnapshot, beat: f64) -> Result<SessionFrame, String> {
    let target = MapPoint::new(
        grid.stamp(),
        Beat::new(beat).map_err(|error| error.to_string())?,
    );
    let BeatGridQuery::Resolved(position) = grid.position_at(target) else {
        return Err("selected source position has no Host target frame".to_owned());
    };
    let MapPosition::Session(frame) = *position.value().value() else {
        return Err("selected Host grid has a different coordinate axis".to_owned());
    };
    Ok(frame)
}

fn source_media_at(
    grid: &BeatGridSnapshot,
    evaluation_rate: NonZeroU32,
    beat: f64,
) -> Result<Option<Duration>, String> {
    let point = MapPoint::new(
        grid.stamp(),
        Beat::new(beat).map_err(|error| error.to_string())?,
    );
    let query = grid.position_at(point);
    let position = match query {
        BeatGridQuery::Resolved(position) => position,
        BeatGridQuery::OutsideDomain => return Ok(None),
        BeatGridQuery::Uncovered { required }
            if required.start() == required.end()
                && grid_beat_at(grid, required.start()).is_ok_and(|start| beat < start) =>
        {
            return Ok(None);
        }
        _ => {
            return Err(format!(
                "source beat {beat:.12} has no declared grid position: {query:?}"
            ));
        }
    };
    let MapPosition::Asset(frame) = *position.value().value() else {
        return Err("declared source grid has a different coordinate axis".to_owned());
    };
    Duration::try_from_secs_f64(f64::from(frame) / f64::from(evaluation_rate.get()))
        .map(Some)
        .map_err(|error| error.to_string())
}

#[cfg(not(target_os = "android"))]
#[kithara::test(native, tokio, multi_thread, serial, timeout(Duration::from_secs(60)))]
#[case::plain_tempo_hold(
    Provider::HlsMp3(HlsProtection::Plain),
    SyncCase { id: "tempo-hold-30hz", ride: TempoRide::Hold(120.0), ..TEMPO_DOWN_30 },
    None
)]
#[case::symphonia_tempo_down(
    Provider::HlsMp3(HlsProtection::Plain),
    TEMPO_DOWN_30,
    Some(kithara::decode::DecoderBackend::Symphonia)
)]
#[case::symphonia_sync_seek_play(
    Provider::HlsMp3(HlsProtection::Plain),
    SYNC_SEEK_PLAY,
    Some(kithara::decode::DecoderBackend::Symphonia)
)]
async fn supplied_grids_bind_consumed_pcm_marks_to_the_host(
    #[case] provider: Provider,
    #[case] case: SyncCase,
    #[case] decoder: Option<kithara::decode::DecoderBackend>,
) {
    let prepared = prepared_sources(provider).await;
    let mut harness = match decoder {
        Some(decoder) => {
            ProductHarness::new_with_decoder(case, &prepared, CUE, Audible::Deck(0), decoder).await
        }
        None => ProductHarness::new(case, &prepared, CUE, Audible::Deck(0)).await,
    };
    if decoder.is_some() {
        harness.assert_decoder_backend(kithara::audio::DecoderBackend::Symphonia);
    }
    harness.run_operations(case).await;
    harness.ride_tempo(case).await;
    let pcm = harness.capture(case).await;
    let mut failures = harness.grid_binding_failures(case).await;
    failures.extend(harness.grid_anchor_failures(case, Some(2)).await);
    drop(harness);
    failures.extend(synchronization_failures(
        "supplementary PCM",
        &[&pcm],
        CHANNELS,
        case.sample_rate,
        case.final_bpm(),
    ));
    assert!(
        failures.is_empty(),
        "direct grid binding failed: {}",
        failures.join("\n")
    );
}

#[kithara::test(native)]
fn ordinary_source_spans_preserve_beat_sides_at_rounded_seams() {
    let rate = NonZeroU32::new(48_000).expect("source rate");
    let ArtifactSource::Value(model) = synthetic_grid() else {
        panic!("declared source grid");
    };
    let mut raw = model.as_raw().clone();
    raw.duration = None;
    raw.beats = [(74, 31.96), (75, 32.38), (76, 32.8)]
        .into_iter()
        .map(|(ordinal, at)| GridBeat {
            at,
            ordinal,
            confidence: None,
        })
        .collect();
    raw.downbeats.clear();
    raw.meter = None;
    let model = BeatGridModel::try_from(raw).expect("rounded source seam");
    let source = source_with_duration(&model, rate, None).expect("declared source geometry");
    let seam = 75.0_f64;
    let before = seam.next_down();
    let position = media_seconds_position(
        source_media_for_beat(&source, before, rate).expect("inverse before seam"),
        rate,
    )
    .expect("asset coordinate before seam");
    let BeatGridQuery::Resolved(kithara::warp::BeatGridRegion::Bounded(region)) =
        source.region_at(MapPoint::new(source.stamp(), position))
    else {
        panic!("bounded source region");
    };
    assert!(before < seam);
    assert_eq!(
        grid_beat_at(&source, region.start()).expect("seam beat"),
        seam
    );
    assert_eq!(
        position,
        region.start(),
        "inverse rounds onto the next cell"
    );
    let MapPosition::Asset(start) = region.start() else {
        panic!("asset region");
    };
    let previous = MapPosition::Asset(
        AssetFrame::new(f64::from(start).next_down()).expect("previous asset coordinate"),
    );
    assert_ne!(
        grid_tempo(&source, previous).expect("previous tempo"),
        grid_tempo(&source, position).expect("next tempo"),
        "exact adjacent slopes must differ"
    );
    for from in [before.next_down(), before, seam, seam.next_up()] {
        for to in [before, seam, seam.next_up(), before + 2.0 / 48_000.0] {
            if to <= from {
                continue;
            }
            for include_endpoints in [false, true] {
                let crosses = if include_endpoints {
                    from <= seam && seam <= to
                } else {
                    from < seam && seam < to
                };
                assert_eq!(
                    ordinary_span_is_local(&source, from, to, rate, include_endpoints)
                        .expect("finite source span"),
                    !crosses,
                    "span {from:?}..{to:?}, endpoints={include_endpoints}"
                );
            }
        }
    }
}

#[kithara::test(native)]
fn direct_grid_binding_rejects_a_frame_a_beat_and_a_missing_origin() {
    let source_rate = NonZeroU32::new(48_000).expect("source rate");
    let output_rate = NonZeroU32::new(44_100).expect("Host rate");
    let anchor = SessionAnchor::new(
        SessionFrame::new(0),
        SessionBeat::default(),
        1.8,
        output_rate,
    )
    .expect("Host anchor");
    let host_grid = BeatGridSnapshot::session(
        BeatGridId::allocate().expect("Host grid id"),
        BeatGridRevision::first(),
        SessionEpoch::new(0),
        anchor,
        None,
    );
    let ArtifactSource::Value(model) = synthetic_grid() else {
        panic!("declared source grid");
    };
    assert_eq!(
        model
            .as_raw()
            .meter
            .expect("declared source meter")
            .origin_beat_ordinal,
        1
    );
    let consumed = SlotSnapshot {
        state: SlotState::Playing,
        mark: Some(SlotMark {
            session: SessionFrame::new(171_500),
            lane: LaneFrame {
                segment: SegmentId::FIRST,
                frame: 171_500,
            },
            position: Duration::from_millis(4_012),
        }),
        source: Some(AudioChunkInfo {
            spec: AudioSpec::new(2, output_rate),
            segment: SegmentId::FIRST,
            media_origin: Some(Duration::from_millis(12)),
            source_span: Some(
                SourceSpan::new(192_576, 192_624, source_rate, 49)
                    .expect("declared consumed native geometry"),
            ),
            lane_frame: 171_500,
            frames: 49,
            ..AudioChunkInfo::default()
        }),
        ..SlotSnapshot::default()
    };
    let binding = check(&model, source_rate, &host_grid, consumed, 0.9, 4, None)
        .expect("declared source and Host grids");
    assert!(binding.failures().is_empty(), "{binding:?}");
    assert_eq!(binding.source_beat, 8.0);
    assert_eq!(binding.host_beat, 7.0);
    assert_eq!(binding.expected_frame, SessionFrame::new(171_500));

    let shifted_mark = SlotSnapshot {
        mark: consumed.mark.map(|mark| SlotMark {
            session: SessionFrame::new(171_501),
            ..mark
        }),
        ..consumed
    };
    let shifted = check(&model, source_rate, &host_grid, shifted_mark, 0.9, 4, None)
        .expect("shifted source mark");
    assert_eq!(shifted.expected_frame, SessionFrame::new(171_500));
    assert_eq!(shifted.mark.session, SessionFrame::new(171_501));
    assert_eq!(shifted.failures().len(), 1);

    let mut wrong_grid = model.as_raw().clone();
    for beat in &mut wrong_grid.beats {
        beat.at += 0.5;
    }
    for downbeat in &mut wrong_grid.downbeats {
        downbeat.at += 0.5;
    }
    let wrong_grid = BeatGridModel::try_from(wrong_grid).expect("a valid misplaced source grid");
    let misplaced = check(&wrong_grid, source_rate, &host_grid, consumed, 0.9, 4, None)
        .expect("one-beat misplaced grid");
    assert_eq!(misplaced.expected_frame, SessionFrame::new(147_000));
    assert_eq!(misplaced.failures().len(), 1);

    let mut one_frame_grid = model.as_raw().clone();
    let frame_seconds = 1.0 / f64::from(output_rate.get());
    for beat in &mut one_frame_grid.beats {
        beat.at += frame_seconds;
    }
    for downbeat in &mut one_frame_grid.downbeats {
        downbeat.at += frame_seconds;
    }
    let one_frame_grid =
        BeatGridModel::try_from(one_frame_grid).expect("a one-frame displaced source grid");
    let misplaced_frame = check(
        &one_frame_grid,
        source_rate,
        &host_grid,
        consumed,
        0.9,
        4,
        None,
    )
    .expect("one-frame displaced source grid");
    assert_eq!(misplaced_frame.expected_frame, SessionFrame::new(171_499));
    assert_eq!(misplaced_frame.failures().len(), 1);

    assert!(
        check(
            &model,
            source_rate,
            &host_grid,
            SlotSnapshot {
                source: consumed.source.map(|info| AudioChunkInfo {
                    media_origin: None,
                    ..info
                }),
                ..consumed
            },
            0.9,
            4,
            None
        )
        .is_err()
    );
    let ignored_origin = check(
        &model,
        source_rate,
        &host_grid,
        SlotSnapshot {
            source: consumed.source.map(|info| AudioChunkInfo {
                media_origin: Some(Duration::ZERO),
                ..info
            }),
            ..consumed
        },
        0.9,
        4,
        None,
    )
    .expect("incorrect raw/media translation");
    assert_eq!(ignored_origin.expected_frame, SessionFrame::new(172_088));
    assert_eq!(ignored_origin.failures().len(), 1);

    let mut missed_crossing = model.as_raw().clone();
    missed_crossing.beats[7].at = 3.75;
    let missed_crossing = BeatGridModel::try_from(missed_crossing).expect("nonuniform source grid");
    let source =
        declared_source_grid(&missed_crossing, source_rate).expect("declared nonuniform geometry");
    assert!(
        ordinary_span_is_local(&source, 0.0, 4.0, source_rate, false)
            .expect("unchanged first half")
    );
    assert!(
        ordinary_span_is_local(&source, 0.0, 6.0, source_rate, false)
            .expect("boundary exactly at interval end")
    );
    assert!(
        !ordinary_span_is_local(&source, 0.0, 8.0, source_rate, false)
            .expect("slope crossing in second half")
    );

    let model = analysed_grid("library_flac_song2");
    let anchor = SessionAnchor::new(
        SessionFrame::new(0),
        SessionBeat::default(),
        100.0 / 60.0,
        source_rate,
    )
    .expect("real Queue Host anchor");
    let host_grid = BeatGridSnapshot::session(
        BeatGridId::allocate().expect("Host grid id"),
        BeatGridRevision::first(),
        SessionEpoch::new(0),
        anchor,
        None,
    );
    let numerator = 6_865_075_725_926_399_u128;
    let denominator = 1_u128 << 32;
    for (at, numerator, denominator, speed) in [
        (
            431_999,
            numerator * 5 - denominator * 4,
            denominator * 5,
            0.8,
        ),
        (432_000, numerator, denominator, 0.7666666666666748),
    ] {
        let span = SourceSpan::approaching(
            numerator,
            NonZeroU128::new(denominator).expect("exact source denominator"),
            ExponentialApproach::new(speed, speed, 0.0).expect("local source rate"),
            0,
            source_rate,
            1,
        )
        .expect("actual nonuniform consumed boundary");
        let position = span.position_at(0).expect("actual consumed mark");
        if at == 432_000 {
            assert_eq!(span.seconds_at(0), Some(33.3));
            assert_eq!(position, Duration::from_nanos(33_299_999_999));
        }
        let lane_frame = u64::try_from(at - 115_200).expect("actual lane frame");
        let consumed = SlotSnapshot {
            state: SlotState::Playing,
            mark: Some(SlotMark {
                session: SessionFrame::new(at),
                lane: LaneFrame {
                    segment: SegmentId::FIRST,
                    frame: lane_frame,
                },
                position,
            }),
            source: Some(AudioChunkInfo {
                spec: AudioSpec::new(2, source_rate),
                segment: SegmentId::FIRST,
                media_origin: Some(Duration::ZERO),
                source_span: Some(span),
                lane_frame,
                frames: 1,
                ..AudioChunkInfo::default()
            }),
            ..SlotSnapshot::default()
        };
        let binding = check(
            &model,
            source_rate,
            &host_grid,
            consumed,
            speed.as_(),
            4,
            None,
        )
        .expect("exact source coordinate survives reporting truncation");
        assert!(binding.failures().is_empty(), "{binding:?}");
        let wrong_speed = if at == 432_000 { 0.8 } else { 0.76666665 };
        assert_eq!(
            check(
                &model,
                source_rate,
                &host_grid,
                consumed,
                wrong_speed,
                4,
                None
            )
            .expect("wrong local interval speed")
            .failures()
            .len(),
            1
        );
        let wrong_mark = SlotSnapshot {
            mark: consumed.mark.map(|mark| SlotMark {
                position: mark.position + Duration::from_nanos(1),
                ..mark
            }),
            ..consumed
        };
        assert!(
            check(
                &model,
                source_rate,
                &host_grid,
                wrong_mark,
                speed.as_(),
                4,
                None
            )
            .is_err()
        );
    }
}

#[kithara::test(native)]
fn direct_grid_binding_checks_the_declared_crossing_cell() {
    let rate = NonZeroU32::new(48_000).expect("source and Host rate");
    let model = analysed_grid("library_flac_newtechno");
    let at = SessionFrame::new(2_127_986);
    let host = SessionAnchor::new(
        at,
        SessionBeat::new(74.0 - 1.0 / 72_000.0).expect("declared Host phase"),
        2.0,
        rate,
    )
    .expect("declared Host law");
    let host_grid = BeatGridSnapshot::session(
        BeatGridId::allocate().expect("Host grid id"),
        BeatGridRevision::first(),
        SessionEpoch::new(0),
        host,
        None,
    );
    let span = SourceSpan::approaching(
        5_780_681_237_936_773,
        NonZeroU128::new(1_u128 << 32).expect("physical source phase"),
        ExponentialApproach::new(0.8799999998245767, 0.8799999998245767, 0.0)
            .expect("actually consumed crossing cell"),
        0,
        rate,
        1,
    )
    .expect("recorded consumed source geometry");
    let consumed = SlotSnapshot {
        state: SlotState::Playing,
        mark: Some(SlotMark {
            session: at,
            lane: LaneFrame {
                segment: SegmentId::FIRST,
                frame: 25_586,
            },
            position: span.position_at(0).expect("exact consumed position"),
        }),
        source: Some(AudioChunkInfo {
            spec: AudioSpec::new(2, rate),
            segment: SegmentId::FIRST,
            media_origin: Some(Duration::ZERO),
            source_span: Some(span),
            lane_frame: 25_586,
            frames: 1,
            ..AudioChunkInfo::default()
        }),
        ..SlotSnapshot::default()
    };
    let reference = Some(SelectedBinding {
        media: Duration::from_secs_f64(28.04 - 0.4 / 72_000.0),
        source_beat: 65.0 - 1.0 / 72_000.0,
        host_beat: 74.0 - 1.0 / 72_000.0,
        frame: at,
    });
    let binding = check(&model, rate, &host_grid, consumed, 0.88, 4, reference)
        .expect("the recorded source and Host grids cover the consumed frame");
    assert_eq!(binding.expected_frame, at);
    assert!(binding.failures().is_empty(), "{binding:?}");
    for wrong_speed in [0.8, 0.92] {
        assert!(
            !check(
                &model,
                rate,
                &host_grid,
                consumed,
                wrong_speed,
                4,
                reference
            )
            .expect("both instantaneous sides are covered")
            .failures()
            .is_empty(),
            "the crossing sample must reject either instantaneous side"
        );
    }

    let ArtifactSource::Value(model) = synthetic_grid() else {
        panic!("declared source grid");
    };
    let mut raw = model.as_raw().clone();
    raw.beats = [0.0, 0.5, 0.75]
        .into_iter()
        .enumerate()
        .map(|(ordinal, at)| GridBeat {
            at,
            ordinal: ordinal.as_(),
            confidence: None,
        })
        .collect();
    raw.downbeats.clear();
    raw.meter = None;
    raw.bpm = 120.0;
    let model = BeatGridModel::try_from(raw).expect("two different declared source slopes");
    let host = SessionAnchor::new(
        SessionFrame::new(0),
        SessionBeat::default(),
        100.0 / 60.0,
        rate,
    )
    .expect("fractional entry Host law");
    let start = SessionFrame::new(8641);
    let reference = Some(SelectedBinding {
        media: Duration::from_nanos(150_020_000),
        source_beat: 0.30004,
        host_beat: f64::from(host.beat_at(start).expect("declared Start Host beat")),
        frame: start,
    });
    let host_grid = BeatGridSnapshot::session(
        BeatGridId::allocate().expect("fractional entry Host id"),
        BeatGridRevision::first(),
        SessionEpoch::new(0),
        host,
        None,
    );
    for (at, numerator, denominator, speed, wrong) in [
        (28_798, 1_199_923, 50, 5.0 / 6.0, [0.77, 5.0 / 12.0]),
        (28_799, 1_799_947, 75, 0.77, [5.0 / 6.0, 5.0 / 12.0]),
        (
            28_800,
            (24_000_u128 << 32) - 1,
            1_u128 << 32,
            5.0 / 12.0,
            [0.77, 5.0 / 6.0],
        ),
    ] {
        let reference = if at == 28_800 {
            Some(SelectedBinding {
                media: Duration::from_millis(500),
                source_beat: 1.0,
                host_beat: 1.0,
                frame: SessionFrame::new(at),
            })
        } else {
            reference
        };
        let phase = physical_phase((
            numerator,
            NonZeroU128::new(denominator).expect("declared physical phase divisor"),
        ))
        .expect("physical source phase");
        let span = SourceSpan::approaching(
            phase.0,
            phase.1,
            ExponentialApproach::new(speed, speed, 0.0).expect("declared cell speed"),
            0,
            rate,
            1,
        )
        .expect("fractional entry consumed cell");
        let mut consumed = consumed;
        let lane_frame = u64::try_from(at - i64::from(start)).expect("source follows Start");
        consumed.mark = Some(SlotMark {
            session: SessionFrame::new(at),
            lane: LaneFrame {
                segment: SegmentId::FIRST,
                frame: lane_frame,
            },
            position: span.position_at(0).expect("fractional cue source position"),
        });
        let source = consumed.source.as_mut().expect("source geometry");
        source.lane_frame = lane_frame;
        source.source_span = Some(span);
        let binding = check(
            &model,
            rate,
            &host_grid,
            consumed,
            speed.as_(),
            4,
            reference,
        )
        .expect("declared fractional cue/Start binding");
        assert_eq!(binding.expected_frame, SessionFrame::new(at));
        assert!(binding.failures().is_empty(), "{binding:?}");
        for wrong_speed in wrong {
            assert_eq!(
                check(
                    &model,
                    rate,
                    &host_grid,
                    consumed,
                    wrong_speed.as_(),
                    4,
                    reference
                )
                .expect("covered wrong cell slope")
                .failures()
                .len(),
                1,
            );
        }
        let mut shifted = consumed;
        shifted.mark.as_mut().expect("consumed mark").session = SessionFrame::new(at + 1);
        assert!(
            !check(&model, rate, &host_grid, shifted, speed.as_(), 4, reference)
                .expect("one-frame displacement remains covered")
                .failures()
                .is_empty(),
        );
    }
}

#[kithara::test(native)]
fn direct_grid_anchor_rejects_a_whole_bar_and_a_wrong_selected_cue() {
    let evaluation_rate = NonZeroU32::new(48_000).expect("grid evaluation rate");
    let output_rate = NonZeroU32::new(44_100).expect("Host rate");
    let host_anchor = SessionAnchor::new(
        SessionFrame::new(0),
        SessionBeat::default(),
        1.8,
        output_rate,
    )
    .expect("Host anchor");
    let host_grid = BeatGridSnapshot::session(
        BeatGridId::allocate().expect("Host grid id"),
        BeatGridRevision::first(),
        SessionEpoch::new(0),
        host_anchor,
        None,
    );
    let ArtifactSource::Value(model) = synthetic_grid() else {
        panic!("declared source grid");
    };
    assert_eq!(
        model
            .as_raw()
            .meter
            .expect("declared source meter")
            .origin_beat_ordinal,
        1
    );
    let frame = SessionFrame::new(171_500);
    let selected = select_nearest(
        &model,
        evaluation_rate,
        &host_grid,
        Duration::from_secs(4),
        frame,
        4,
    )
    .expect("selected source position");
    assert_eq!(selected.media, Duration::from_secs(4));
    assert_eq!(selected.source_beat, 8.0);
    assert_eq!(selected.host_beat, 7.0);
    assert_eq!(selected.frame, frame);
    assert!(
        selected
            .command_failures(&model, evaluation_rate, &host_grid, selected.media, frame)
            .expect("exact selected command")
            .is_empty()
    );
    let prior = Duration::from_secs(2);
    let requested = relocation_request(SYNC_SEEK_PLAY.order, true, selected.media, Ok(prior))
        .expect("Seek request survives the final Play");
    let after_play = select_nearest(&model, evaluation_rate, &host_grid, requested, frame, 4)
        .expect("requested Seek selects its own source pair");
    assert_eq!(after_play.source_beat, 8.0);
    assert!(
        after_play
            .command_failures(&model, evaluation_rate, &host_grid, selected.media, frame)
            .expect("requested Seek command after Play")
            .is_empty()
    );
    assert_eq!(
        after_play
            .command_failures(&model, evaluation_rate, &host_grid, prior, frame)
            .expect("prior projection cannot replace the requested Seek")
            .len(),
        1
    );
    let sync_request = relocation_request(SYNC_SEEK_PLAY.order, false, selected.media, Ok(prior))
        .expect("earlier Sync uses its pre-emission projection");
    let sync = select_nearest(&model, evaluation_rate, &host_grid, sync_request, frame, 4)
        .expect("Sync selects the projected source pair");
    assert_eq!(sync.source_beat, 4.0);
    let consumed = SlotSnapshot {
        state: SlotState::Playing,
        mark: Some(SlotMark {
            session: frame,
            lane: LaneFrame {
                segment: SegmentId::FIRST,
                frame: 171_500,
            },
            position: Duration::from_millis(4_012),
        }),
        source: Some(AudioChunkInfo {
            spec: AudioSpec::new(2, output_rate),
            segment: SegmentId::FIRST,
            media_origin: Some(Duration::from_millis(12)),
            source_span: Some(
                SourceSpan::new(192_576, 192_624, evaluation_rate, 49)
                    .expect("declared consumed native geometry"),
            ),
            lane_frame: 171_500,
            frames: 49,
            ..AudioChunkInfo::default()
        }),
        ..SlotSnapshot::default()
    };
    assert!(
        selected
            .consumption_failures(
                &model,
                evaluation_rate,
                &host_grid,
                consumed,
                SegmentId::FIRST
            )
            .expect("exact selected consumption")
            .is_empty()
    );

    for seconds in [2, 6] {
        let media = Duration::from_secs(seconds);
        let wrong = SlotSnapshot {
            mark: consumed.mark.map(|mark| SlotMark {
                position: media + Duration::from_millis(12),
                ..mark
            }),
            source: consumed.source.map(|info| AudioChunkInfo {
                source_span: Some(
                    SourceSpan::new(
                        seconds * 48_000 + 576,
                        seconds * 48_000 + 624,
                        evaluation_rate,
                        49,
                    )
                    .expect("wrong whole-bar consumed geometry"),
                ),
                ..info
            }),
            ..consumed
        };
        assert!(
            check(&model, evaluation_rate, &host_grid, wrong, 0.9, 4, None)
                .expect("whole-bar phase-only binding")
                .failures()
                .is_empty()
        );
        assert_eq!(
            selected
                .consumption_failures(&model, evaluation_rate, &host_grid, wrong, SegmentId::FIRST)
                .expect("wrong whole-bar consumption")
                .len(),
            1
        );
        assert_eq!(
            selected
                .command_failures(&model, evaluation_rate, &host_grid, media, frame)
                .expect("wrong whole-bar command target")
                .len(),
            1
        );
    }
    let next_frame = SessionFrame::new(171_501);
    assert_eq!(
        selected
            .command_failures(
                &model,
                evaluation_rate,
                &host_grid,
                selected.media,
                next_frame
            )
            .expect("wrong command moment")
            .len(),
        1
    );
    let one_frame_media =
        selected.media + Duration::from_secs_f64(1.0 / f64::from(output_rate.get()));
    assert_eq!(
        selected
            .command_failures(&model, evaluation_rate, &host_grid, one_frame_media, frame)
            .expect("one-frame command target displacement")
            .len(),
        1
    );
    let shifted = SlotSnapshot {
        mark: consumed.mark.map(|mark| SlotMark {
            session: next_frame,
            ..mark
        }),
        ..consumed
    };
    assert_eq!(
        selected
            .consumption_failures(
                &model,
                evaluation_rate,
                &host_grid,
                shifted,
                SegmentId::FIRST
            )
            .expect("one-frame consumption displacement")
            .len(),
        1
    );
    let wrong_segment = SlotSnapshot {
        source: consumed.source.map(|info| AudioChunkInfo {
            segment: SegmentId::FIRST.next(),
            ..info
        }),
        mark: consumed.mark.map(|mark| SlotMark {
            lane: LaneFrame {
                segment: SegmentId::FIRST.next(),
                ..mark.lane
            },
            ..mark
        }),
        ..consumed
    };
    assert_eq!(
        selected
            .consumption_failures(
                &model,
                evaluation_rate,
                &host_grid,
                wrong_segment,
                SegmentId::FIRST
            )
            .expect("foreign consumed segment")
            .len(),
        1
    );
    assert!(
        selected
            .consumption_failures(
                &model,
                evaluation_rate,
                &host_grid,
                SlotSnapshot {
                    source: consumed.source.map(|info| AudioChunkInfo {
                        media_origin: None,
                        ..info
                    }),
                    ..consumed
                },
                SegmentId::FIRST
            )
            .is_err()
    );
    let advanced = SlotSnapshot {
        mark: consumed.mark.map(|mark| SlotMark {
            session: SessionFrame::new(240_100),
            lane: LaneFrame {
                frame: 240_100,
                ..mark.lane
            },
            position: Duration::from_millis(5_412),
        }),
        ..consumed
    };
    assert!(
        selected
            .consumption_failures(
                &model,
                evaluation_rate,
                &host_grid,
                advanced,
                SegmentId::FIRST
            )
            .expect("consumption advances from the frozen source/Host pair")
            .is_empty()
    );

    let tied = select_nearest(
        &model,
        evaluation_rate,
        &host_grid,
        Duration::from_secs(5),
        frame,
        4,
    )
    .expect("equidistant source positions");
    assert_eq!(
        tied.media,
        Duration::from_secs(4),
        "nearest jump resolves an exact tie backward"
    );
    let nonnegative = select_nearest(
        &model,
        evaluation_rate,
        &host_grid,
        Duration::ZERO,
        SessionFrame::new(49_000),
        4,
    )
    .expect("nearest nonnegative source position");
    assert_eq!(nonnegative.media, Duration::from_millis(1_500));
    assert_eq!(nonnegative.source_beat, 3.0);

    let mut shifted = model.as_raw().clone();
    shifted.duration = None;
    for beat in &mut shifted.beats {
        beat.ordinal += 2;
        beat.at += 1.0;
    }
    for downbeat in &mut shifted.downbeats {
        downbeat.beat_ordinal += 2;
        downbeat.at += 1.0;
    }
    shifted
        .meter
        .as_mut()
        .expect("declared fixture meter")
        .origin_beat_ordinal += 2;
    let shifted = BeatGridModel::try_from(shifted).expect("shifted declared source grid");
    let head_grid = declared_source_grid(&shifted, evaluation_rate).expect("shifted grid head");
    assert_eq!(
        source_media_at(&head_grid, evaluation_rate, 1.0).expect("physical extrapolated head"),
        Some(Duration::from_millis(500))
    );
    assert!(source_media_at(&head_grid, evaluation_rate, 26.0).is_err());
    let declared = select_nearest(
        &shifted,
        evaluation_rate,
        &host_grid,
        Duration::from_secs(1),
        SessionFrame::new(49_000),
        4,
    )
    .expect("nearest mapped source position within its head");
    assert_eq!(declared.media, Duration::from_millis(500));
    assert_eq!(declared.source_beat, 1.0);

    let entry = select_entry(
        &model,
        evaluation_rate,
        &host_grid,
        Duration::from_millis(5_250),
        frame,
        4,
    )
    .expect("selected paused entry");
    assert_eq!(entry.media, Duration::from_millis(6_500));
    assert_eq!(entry.source_beat, 13.0);
    assert_eq!(entry.host_beat, 8.0);
    assert_eq!(entry.frame, SessionFrame::new(196_000));
    assert!(
        entry
            .command_failures(
                &model,
                evaluation_rate,
                &host_grid,
                entry.media,
                entry.frame
            )
            .expect("exact selected entry")
            .is_empty()
    );
    let entry_consumed = SlotSnapshot {
        mark: consumed.mark.map(|mark| SlotMark {
            session: SessionFrame::new(196_000),
            lane: LaneFrame {
                frame: 196_000,
                ..mark.lane
            },
            position: Duration::from_millis(6_512),
        }),
        ..consumed
    };
    assert!(
        entry
            .consumption_failures(
                &model,
                evaluation_rate,
                &host_grid,
                entry_consumed,
                SegmentId::FIRST
            )
            .expect("different absolute source and Host ordinals remain bound")
            .is_empty()
    );
    assert_eq!(
        entry
            .command_failures(
                &model,
                evaluation_rate,
                &host_grid,
                Duration::from_secs(4),
                entry.frame
            )
            .expect("wrong paused selected cue")
            .len(),
        1
    );
    let after = select_entry(
        &model,
        evaluation_rate,
        &host_grid,
        Duration::from_millis(5_250),
        SessionFrame::new(196_001),
        4,
    )
    .expect("entry after the previous Host downbeat");
    assert_eq!(after.source_beat, 13.0);
    assert_eq!(after.host_beat, 12.0);
    assert_eq!(after.frame, SessionFrame::new(294_000));

    let item = kithara::events::TrackId::allocate();
    let (posts, _mailbox) = kithara_command::mailbox::<(), ()>();
    let seq = posts.post(()).expect("real command identity").seq();
    let wrong_seq = posts.post(()).expect("different command identity").seq();
    let track = kithara::link::LinkedSnapshot {
        track: kithara::play::TrackSnapshot {
            output: kithara::play::SlotOutputSnapshot::default(),
            effects: Vec::new(),
            item,
            slot: None,
            status: kithara::play::TrackStatus::Playing { since: frame },
            speed: 1.0,
            position: Duration::ZERO,
            duration: Some(Duration::from_secs(12)),
            abr: None,
            metadata: kithara::decode::TrackMetadata::default(),
            mark: consumed.mark,
            engine_latency: kithara::signal::FrameCount::new(0),
            ring_depth: kithara::signal::FrameCount::new(0),
            lane_room: 1,
            pending_lane: false,
            attached: true,
            declick: kithara::signal::FrameCount::new(0),
        },
        sync: kithara::link::SyncStatus::WaitingForGrid {
            required: Duration::ZERO,
        },
    };
    let mut observation = LinkObservation {
        loads: vec![(item, seq)],
        owner_inputs: vec![
            LinkOwnerInput {
                item: Some(item),
                at: None,
                trajectory: None,
                kind: LinkOwnerInputKind::Grid {
                    answer: Box::new(GridAnswer {
                        item,
                        load: seq,
                        model: Ok(shifted.clone()),
                    }),
                },
            },
            LinkOwnerInput {
                item: None,
                at: Some(frame),
                trajectory: None,
                kind: LinkOwnerInputKind::Sync { on: true },
            },
        ],
        sync_requests: vec![LinkSyncRequest {
            on: true,
            seq: None,
            input: 1,
            tracks: vec![track.clone()],
            placements: 0..0,
        }],
        placements: vec![LinkPlacement {
            item,
            seq,
            aligned: true,
            to: selected.media,
            at: frame,
            now: frame,
            delivery: kithara::signal::FrameCount::new(0),
            before: track.track.clone(),
            consumed,
            planned: Ok((Duration::ZERO, 1.0)),
            input: Some(1),
        }],
        ..LinkObservation::default()
    };
    assert_eq!(
        relocation_count(&observation, item, &shifted, PLAY_SYNC_SEEK, None)
            .expect("actual pending Sync is answered by the later Seek"),
        Some(1)
    );
    assert_eq!(
        relocation_count(&observation, item, &shifted, PLAY_SYNC_SEEK, Some(2))
            .expect("explicit relocation count remains strict"),
        Some(2)
    );
    observation.placements[0].now = frame - kithara::signal::FrameCount::new(1);
    assert!(relocation_count(&observation, item, &shifted, PLAY_SYNC_SEEK, None).is_err());
    observation.placements[0].now = frame;
    observation.placements[0].input = Some(0);
    assert!(relocation_count(&observation, item, &shifted, PLAY_SYNC_SEEK, None).is_err());
    observation.placements[0].input = Some(1);
    observation.sync_requests[0].tracks.clear();
    assert!(relocation_count(&observation, item, &shifted, PLAY_SYNC_SEEK, None).is_err());
    observation.sync_requests[0].tracks.push(track);
    for beat in [
        shifted.as_raw().beats.first(),
        shifted.as_raw().beats.last(),
    ] {
        let required = Duration::from_secs_f64(beat.expect("observed inclusive endpoint").at);
        observation.sync_requests[0].tracks[0].sync =
            kithara::link::SyncStatus::WaitingForGrid { required };
        assert!(relocation_count(&observation, item, &shifted, PLAY_SYNC_SEEK, None).is_err());
    }
    observation.sync_requests[0].tracks[0].sync = kithara::link::SyncStatus::On;
    assert!(relocation_count(&observation, item, &shifted, PLAY_SYNC_SEEK, None).is_err());
    observation.sync_requests[0].tracks[0].sync = kithara::link::SyncStatus::WaitingForGrid {
        required: Duration::ZERO,
    };
    observation.loads[0].1 = wrong_seq;
    assert!(relocation_count(&observation, item, &shifted, PLAY_SYNC_SEEK, None).is_err());
    observation.loads[0].1 = seq;
    observation.sync_requests[0].seq = Some(seq);
    observation.sync_requests[0].tracks[0].sync = kithara::link::SyncStatus::On;
    observation.sync_requests[0].placements = 0..1;
    observation.placements[0].input = Some(1);
    assert_eq!(
        relocation_count(&observation, item, &shifted, PLAY_SYNC_SEEK, None)
            .expect("admitted initial Sync still requires two relocations"),
        Some(2)
    );
    observation.sync_requests[0].seq = Some(wrong_seq);
    assert!(relocation_count(&observation, item, &shifted, PLAY_SYNC_SEEK, None).is_err());
}

#[kithara::test(native)]
fn confirmed_targets_and_consumed_laws_have_separate_clocks() {
    let item = kithara::events::TrackId::allocate();
    let rate = NonZeroU32::new(48_000).expect("sample rate");
    let (posts, _mailbox) = kithara_command::mailbox::<(), ()>();
    let cue_seq = posts.post(()).expect("cue identity").seq();
    let old_seq = posts.post(()).expect("old law identity").seq();
    let future_seq = posts.post(()).expect("future law identity").seq();
    let start = SlotSnapshot {
        state: SlotState::Playing,
        mark: Some(SlotMark {
            session: SessionFrame::new(96),
            lane: LaneFrame {
                segment: SegmentId::FIRST,
                frame: 96,
            },
            position: Duration::from_millis(2),
        }),
        source: Some(AudioChunkInfo {
            spec: AudioSpec::new(2, rate),
            segment: SegmentId::FIRST,
            media_origin: Some(Duration::ZERO),
            ..AudioChunkInfo::default()
        }),
        ..SlotSnapshot::default()
    };
    let mark = start.mark.expect("consumed mark");
    let before = kithara::play::TrackSnapshot {
        output: kithara::play::SlotOutputSnapshot::default(),
        effects: Vec::new(),
        item,
        slot: None,
        status: kithara::play::TrackStatus::Playing {
            since: SessionFrame::new(0),
        },
        speed: 1.0,
        position: Duration::from_millis(2),
        duration: Some(Duration::from_secs(1)),
        abr: None,
        metadata: kithara::decode::TrackMetadata::default(),
        mark: Some(mark),
        engine_latency: kithara::signal::FrameCount::new(0),
        ring_depth: kithara::signal::FrameCount::new(0),
        lane_room: 2,
        pending_lane: false,
        attached: true,
        declick: kithara::signal::FrameCount::new(0),
    };
    let command = |seq, at, speed| LinkSpeed {
        item,
        seq,
        curve: SpeedCurve::Constant(speed),
        at: When::At(SessionFrame::new(at)),
        landing: None,
        before: before.clone(),
        consumed: Some(start),
        sample_rate: Some(rate),
        input: None,
        host_input: None,
        planned: None,
    };
    let mut observation = LinkObservation {
        cue_curves: vec![LinkCue {
            item,
            seq: cue_seq,
            position: Duration::ZERO,
            curve: SpeedCurve::Constant(1.0),
            before: before.clone(),
            consumed: Some(start),
            host_input: None,
            native: Some(LinkReceiptOrigin::Lane),
            segment: Some(SegmentId::FIRST),
        }],
        ready_cues: vec![(item, cue_seq)],
        speeds: vec![command(future_seq, 192, 1.25)],
        ..LinkObservation::default()
    };
    let check_config = |observation: &LinkObservation, speed| {
        let curves = applied_curves(observation, item, mark, rate).expect("actual outcomes");
        configured_speed_failures(observation, item, mark.lane.segment, &curves, speed)
            .expect("confirmed target")
    };
    for at in [95, 96, 97] {
        observation.speeds[0].at = When::At(SessionFrame::new(at));
        if at < i64::from(mark.session) {
            assert_eq!(
                applied_curves(&observation, item, mark, rate).err(),
                Some(format!("speed {future_seq:?} has no actual Lane outcome")),
            );
        } else {
            assert!(check_config(&observation, 1.0).is_empty());
            assert_eq!(check_config(&observation, 1.25).len(), 1);
        }
    }
    observation.speeds[0].at = When::At(SessionFrame::new(192));
    assert!(check_config(&observation, 1.0).is_empty());
    assert_eq!(check_config(&observation, 1.25).len(), 1);
    observation.placement_receipts.push((
        item,
        LinkReceiptOrigin::Lane,
        kithara::play::Settled::Rejected {
            seq: future_seq,
            reason: kithara_command::Rejection::Stale,
        },
    ));
    assert!(check_config(&observation, 1.0).is_empty());
    assert_eq!(check_config(&observation, 1.25).len(), 1);
    observation.placement_receipts[0].2 = kithara::play::Settled::Applied {
        seq: future_seq,
        at: SessionFrame::new(192),
    };
    assert!(check_config(&observation, 1.25).is_empty());
    assert_eq!(check_config(&observation, 1.0).len(), 1);
    let mut old = command(old_seq, 48, 1.0);
    let old_mark = SlotMark {
        session: SessionFrame::new(24),
        lane: LaneFrame {
            segment: SegmentId::FIRST,
            frame: 24,
        },
        position: Duration::from_micros(500),
    };
    old.before.mark = Some(old_mark);
    old.before.position = old_mark.position;
    old.consumed = Some(SlotSnapshot {
        mark: Some(old_mark),
        ..start
    });
    observation.speeds.insert(0, old);
    observation.placement_receipts.push((
        item,
        LinkReceiptOrigin::Lane,
        kithara::play::Settled::Applied {
            seq: old_seq,
            at: SessionFrame::new(48),
        },
    ));
    let curves = applied_curves(&observation, item, mark, rate).expect("both actual laws");
    assert_eq!(curves.len(), 2);
    assert!(check_config(&observation, 1.25).is_empty());
    assert_eq!(check_config(&observation, 1.1).len(), 1);
    for (end, advanced) in [(191, 95.0), (192, 96.0), (193, 97.25)] {
        let projected = projected_media(
            &observation,
            item,
            &curves,
            start,
            SessionFrame::new(end),
            rate,
        )
        .expect("consumed projection crosses actual F");
        let expected = Duration::try_from_secs_f64(0.002 + advanced / 48_000.0)
            .expect("expected media position");
        assert_eq!(projected, expected, "consumed frame {end}");
    }
    observation.ready_cues.clear();
    observation.speeds.clear();
    assert!(configured_speed_failures(&observation, item, mark.lane.segment, &[], 1.0).is_err());

    let ArtifactSource::Value(model) = synthetic_grid() else {
        panic!("declared source grid");
    };
    let geometry = SourceGeometry::new(&model, rate, None).expect("declared source geometry");
    let host = TempoTrajectory::new(
        TempoStep {
            frame: SessionFrame::new(0),
            beat: SessionBeat::default(),
            tempo: Tempo::new(120.0).expect("Host tempo"),
        },
        NonZeroU16::new(4).expect("Host meter"),
        rate,
    );
    let unity = ExponentialApproach::new(1.0, 1.0, 0.0).expect("declared unity law");
    let wrong = ExponentialApproach::new(2.0, 2.0, 0.0).expect("wrong consumed law");
    let mut terminal = command(old_seq, 0, 1.0);
    terminal.planned = Some(Ok((Duration::ZERO, 1.0)));
    terminal.landing = Some((Duration::ZERO, SessionFrame::new(0)));
    terminal.curve = SpeedCurve::ApproachSteps(vec![(0, unity), (96, wrong), (97, unity)].into());
    let applied = AppliedCurve {
        command: &terminal,
        at: SessionFrame::new(0),
    };
    let verify = |end| {
        applied
            .verify_prefix(
                &geometry,
                &host,
                std::ops::Bound::Included(SessionFrame::new(end)),
                1.0,
                false,
            )
            .expect("finite consumed source prefix")
            .2
    };
    assert!(
        verify(96).is_empty(),
        "a zero-length horizon does not consume the upcoming law"
    );
    for end in [97, 98] {
        let failures = verify(end);
        assert!(
            failures
                .iter()
                .any(|failure| failure.contains("analytic law")),
            "the first consumed wrong sample must fail the exact law at horizon {end}: {failures:?}"
        );
        assert!(
            failures
                .iter()
                .any(|failure| failure.contains("partial area")),
            "the last positive interval must fail physical source area at horizon {end}: {failures:?}"
        );
    }

    let mut raw = model.as_raw().clone();
    raw.beats = [0.0, 0.5, 0.75]
        .into_iter()
        .enumerate()
        .map(|(ordinal, at)| GridBeat {
            at,
            ordinal: i64::try_from(ordinal).expect("fixture ordinal"),
            confidence: None,
        })
        .collect();
    raw.downbeats.clear();
    raw.meter = None;
    let model = BeatGridModel::try_from(raw).expect("two source slopes");
    let geometry = SourceGeometry::new(&model, rate, None).expect("source crossing geometry");
    let source_grid = sync_source_grid(&model, rate, None).expect("native source geometry");
    let host = TempoTrajectory::new(
        TempoStep {
            frame: SessionFrame::new(0),
            beat: SessionBeat::new(-1.0e-9).expect("Host phase before the source crossing"),
            tempo: Tempo::new(120.0).expect("Host tempo"),
        },
        NonZeroU16::new(4).expect("Host meter"),
        rate,
    );
    let start = SessionFrame::new(23_998);
    let media = Duration::from_nanos(499_958_333);
    let (curve, end) = corrected_speed_curve(
        &host,
        &source_grid,
        media,
        start,
        1.0,
        LinkConfig::default().epsilon(),
        true,
    )
    .expect("finite phase correction");
    let parts = analytic_parts(&curve).expect("analytic correction");
    assert_eq!(end, 2, "phase correction ends before the source bridge");
    assert!(
        parts
            .windows(2)
            .any(|pair| pair[0].0 == 2 && pair[0].1.time_constant() == 0.0 && pair[1].0 == 3),
        "fixture must cross the source slope after correction end: {parts:?}"
    );
    terminal.at = When::At(start);
    terminal.curve = curve;
    terminal.planned = Some(Ok((media, 1.0)));
    terminal.landing = None;
    let applied = AppliedCurve {
        command: &terminal,
        at: start,
    };
    let proof = applied
        .verify_prefix(
            &geometry,
            &host,
            std::ops::Bound::Included(start + kithara::signal::FrameCount::new(4)),
            1.0,
            true,
        )
        .expect("source crossing prefix");
    assert_eq!(proof.1, Some(2), "source bridge must not extend correction");
    assert!(proof.2.is_empty(), "valid source bridge: {:?}", proof.2);

    let SpeedCurve::ApproachSteps(parts) = &mut terminal.curve else {
        panic!("source crossing has analytic parts");
    };
    let law = &mut Arc::make_mut(parts)[1].1;
    *law = ExponentialApproach::new(
        law.initial() + 0.001,
        law.target() + 0.001,
        law.time_constant(),
    )
    .expect("wrong terminal law");
    let applied = AppliedCurve {
        command: &terminal,
        at: start,
    };
    let proof = applied
        .verify_prefix(
            &geometry,
            &host,
            std::ops::Bound::Included(start + kithara::signal::FrameCount::new(4)),
            1.0,
            true,
        )
        .expect("wrong terminal remains a representable prefix");
    assert!(
        proof
            .2
            .iter()
            .any(|failure| failure.contains("analytic law")),
        "the terminal frame must retain its exact law check: {:?}",
        proof.2
    );
}

#[kithara::test]
fn completed_host_history_checks_early_cells_and_prior_retimes() {
    let rate = NonZeroU32::new(48_000).expect("Host rate");
    let initial = TempoTrajectory::new(
        TempoStep {
            frame: SessionFrame::new(0),
            beat: SessionBeat::default(),
            tempo: Tempo::new(100.0).expect("initial tempo"),
        },
        NonZeroU16::new(4).expect("meter"),
        rate,
    );
    let mut complete = initial.clone();
    complete
        .push(
            SessionFrame::new(48_000),
            Tempo::new(120.0).expect("retarget"),
        )
        .expect("real earlier tempo step");
    let snapshot = |at| {
        BeatGridSnapshot::session(
            BeatGridId::allocate().expect("published Host grid id"),
            BeatGridRevision::first(),
            SessionEpoch::new(0),
            complete.anchor_at(at),
            None,
        )
    };
    let mut grids: Vec<_> = [0, 24_000, 96_000]
        .into_iter()
        .map(|frame| {
            let at = SessionFrame::new(frame);
            (at, snapshot(at))
        })
        .collect();
    let mut observation = LinkObservation {
        owner_inputs: vec![LinkOwnerInput {
            item: None,
            at: Some(SessionFrame::new(48_000)),
            trajectory: Some(complete.clone()),
            kind: LinkOwnerInputKind::Retime,
        }],
        ..LinkObservation::default()
    };
    let mut failures = Vec::new();
    assert_eq!(
        published_host_history(&observation, &grids, rate, &mut failures),
        Ok(0)
    );
    assert!(failures.is_empty());

    grids[0].1 = BeatGridSnapshot::session(
        BeatGridId::allocate().expect("wrong first grid id"),
        BeatGridRevision::first(),
        SessionEpoch::new(0),
        SessionAnchor::new(
            SessionFrame::new(0),
            SessionBeat::new(1.0).expect("wrong initial beat"),
            100.0 / 60.0,
            rate,
        )
        .expect("valid misplaced early grid"),
        None,
    );
    published_host_history(&observation, &grids, rate, &mut failures)
        .expect("all snapshots are queryable");
    assert_eq!(
        failures,
        [
            "Host execution at SessionFrame(0): complete owner trajectory beat 0 differs from actual published beat 1 or reached tempo"
        ]
    );
    grids[0].1 = snapshot(SessionFrame::new(0));
    failures.clear();

    observation.owner_inputs[0].trajectory = Some(initial);
    published_host_history(&observation, &grids, rate, &mut failures)
        .expect("the incomplete trajectory is queryable");
    assert_eq!(failures.len(), 1);
    assert!(failures[0].starts_with("Host execution at SessionFrame(96000):"));
    observation.owner_inputs[0].trajectory = Some(complete);
    failures.clear();
    assert_eq!(
        published_host_history(&observation, &[], rate, &mut failures),
        Err("law has no actual published Host execution history".to_owned()),
    );
    assert_eq!(
        published_host_history(
            &observation,
            &grids,
            NonZeroU32::new(44_100).expect("other clock"),
            &mut failures,
        ),
        Err("published Host proof and consumed source have different clocks".to_owned()),
    );
    observation.owner_inputs.clear();
    assert_eq!(
        published_host_history(&observation, &grids, rate, &mut failures),
        Err("law has no real complete Host trajectory input".to_owned()),
    );
}

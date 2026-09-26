use std::ops::Range;

use kithara_signal::SessionFrame;
use kithara_warp::{
    AssetFrame, Beat, BeatAlignment, BeatEvidence, BeatGridQuery, BeatGridSnapshot,
    BeatGridUnavailable, BeatOrdinal, CoordinateError, GridProjectionError, MapPoint, MapPosition,
    MapRegion, Meter, PresentationFrontier, WarpMap, WarpMapRevision, WarpPlan, WarpPlanError,
};
use num_traits::ToPrimitive;

use crate::{AlignmentSource, SyncError};

/// Why a member cannot be placed on the group's beats now.
#[derive(Debug)]
pub(super) enum Missing {
    /// A later grid revision may publish the coverage the placement needs.
    Coverage(MapRegion),
    /// The placement is refused on the current facts.
    Refused(SyncError),
}

impl From<SyncError> for Missing {
    fn from(error: SyncError) -> Self {
        Self::Refused(error)
    }
}

impl From<GridProjectionError> for Missing {
    fn from(error: GridProjectionError) -> Self {
        Self::Refused(SyncError::Projection(Box::new(error)))
    }
}

/// The member beat and group beat that sound together, and the session frame
/// at which they do.
#[derive(Clone, Copy)]
pub(super) struct Placement {
    pub(super) alignment: BeatAlignment,
    pub(super) activation: SessionFrame,
}

/// The first admissible beat or proven bar must fit in this finite musical
/// window. One complete owner bar plus one beat after `start` also covers a
/// member-supplied meter when the owner has no meter of its own.
pub(super) fn entry_window(
    owner: &BeatGridSnapshot,
    member: &BeatGridSnapshot,
    source: AlignmentSource,
    start: SessionFrame,
) -> Result<Range<SessionFrame>, Missing> {
    let heard = match source {
        AlignmentSource::Prepared(cue) | AlignmentSource::Cued(cue) => MapPosition::Asset(cue),
        AlignmentSource::Audible { frontier, .. } => {
            MapPosition::Asset(asset_frame(member, frontier.source())?)
        }
    };
    let member_beat = whole_beat(member, member_beat_at_or_next(member, heard)?)?;
    let member_meter = bar_of(member, member_beat, heard)?;
    let at = MapPosition::Session(start);
    let owner_beat = owner_beat_at(owner, at)?;
    let owner_meter = owner_bar(owner, owner_beat, member_meter, at)?;
    let last = Beat::new(f64::from(owner_beat) + bar_length(owner_meter) + 1.0)
        .map_err(|_| outside(owner))?;
    let end = session_frame(owner, last, at)?;
    if end <= start {
        return Err(outside(owner).into());
    }
    Ok(start..end)
}

/// Places `member` on the first admissible `owner` beat inside `window`.
///
/// A member whose grid proves its bars enters on a downbeat in its own bar
/// phase; one whose grid proves only beats enters on a whole beat. A silent
/// member starts on its first such beat at or after its cue, or exactly on a
/// cue it must start from; an audible one keeps playing and is carried to the
/// member beat its live stream reaches no earlier than the activation.
pub(super) fn place(
    owner: &BeatGridSnapshot,
    member: &BeatGridSnapshot,
    source: AlignmentSource,
    window: &Range<SessionFrame>,
) -> Result<Placement, Missing> {
    place_with_previous(owner, member, source, window, None)
}

/// Re-enters the parent's beat phase from a mapped audible lane. The previous
/// plan, rather than a scalar rate, gives the source actually playing at the
/// future activation through a tempo ramp.
pub(super) fn place_mapped(
    owner: &BeatGridSnapshot,
    member: &BeatGridSnapshot,
    source: AlignmentSource,
    window: &Range<SessionFrame>,
    previous: &WarpPlan,
) -> Result<Placement, Missing> {
    place_with_previous(owner, member, source, window, Some(previous))
}

fn place_with_previous(
    owner: &BeatGridSnapshot,
    member: &BeatGridSnapshot,
    source: AlignmentSource,
    window: &Range<SessionFrame>,
    previous: Option<&WarpPlan>,
) -> Result<Placement, Missing> {
    let (owner_beat, member_beat) = match source {
        AlignmentSource::Prepared(cue) => {
            let cue = MapPosition::Asset(cue);
            let first = whole_beat(member, member_beat_at_or_next(member, cue)?)?;
            let member_meter = bar_of(member, first, cue)?;
            let member_beat = next_downbeat(member, first, member_meter)?;
            let owner_beat = enter(owner, member_beat, member_meter, window.start)?;
            (owner_beat, member_beat)
        }
        AlignmentSource::Cued(cue) => {
            let cue = MapPosition::Asset(cue);
            let member_beat = resolve(
                member,
                member.beat_at(MapPoint::new(member.stamp(), cue)),
                cue,
            )?;
            let member_beat = *member_beat.value().value();
            let member_meter = bar_of(member, member_beat, cue)?;
            let owner_beat = enter(owner, member_beat, member_meter, window.start)?;
            (owner_beat, member_beat)
        }
        AlignmentSource::Audible { frontier, speed } => {
            if !speed.is_finite() || speed <= 0.0 {
                return Err(SyncError::from(CoordinateError::NonInvertibleRate).into());
            }
            let past_frontier = i64::from(frontier.output())
                .checked_add(1)
                .map(SessionFrame::new)
                .ok_or_else(|| outside(owner))?;
            let lower = window.start.max(past_frontier);
            let at = MapPosition::Session(lower);
            let heard = MapPosition::Asset(asset_frame(member, frontier.source())?);
            let heard_beat = whole_beat(member, member_beat_at_or_next(member, heard)?)?;
            let member_meter = bar_of(member, heard_beat, heard)?;
            let under = owner_beat_at(owner, at)?;
            let owner_meter = owner_bar(owner, under, member_meter, at)?;
            let owner_beat = first_boundary(owner, under, lower, owner_meter, 0.0)?;
            let activation = session_frame(owner, owner_beat, at)?;
            let live = match previous {
                Some(plan) => mapped_live_source(member, frontier, plan, activation)?,
                None => live_source(owner, member, frontier, speed, activation)?,
            };
            let live = MapPosition::Asset(live);
            let live_beat = whole_beat(member, member_beat_at_or_next(member, live)?)?;
            let owner_phase = bar_phase(owner_beat, owner_meter).ok_or_else(|| outside(owner))?;
            let member_beat =
                with_phase(live_beat, member_meter, owner_phase).ok_or_else(|| outside(member))?;
            (owner_beat, member_beat)
        }
    };
    let end = window.end;
    let activation = session_frame(owner, owner_beat, MapPosition::Session(end))?;
    if activation >= end {
        return Err(Missing::Refused(SyncError::NoAdmissibleBoundary {
            member_id: member.id(),
            first: activation,
            end,
        }));
    }
    Ok(Placement {
        alignment: BeatAlignment::new(
            MapPoint::new(member.stamp(), member_beat),
            MapPoint::new(owner.stamp(), owner_beat),
        ),
        activation,
    })
}

/// The first owner beat at or after `lower` in the bar phase of
/// `member_beat`.
fn enter(
    owner: &BeatGridSnapshot,
    member_beat: Beat,
    member_meter: Option<Meter>,
    lower: SessionFrame,
) -> Result<Beat, Missing> {
    let at = MapPosition::Session(lower);
    let under = owner_beat_at(owner, at)?;
    let owner_meter = owner_bar(owner, under, member_meter, at)?;
    let phase = bar_phase(member_beat, member_meter).ok_or_else(|| outside(owner))?;
    first_boundary(owner, under, lower, owner_meter, phase)
}

/// The first owner beat in bar `phase` whose session frame is at or after
/// `lower`, where `under` is the beat playing at `lower`.
///
/// A boundary a fraction of a frame before `lower` rounds onto `lower`, so the
/// beat playing there may already lie past it; that boundary still counts.
fn first_boundary(
    owner: &BeatGridSnapshot,
    under: Beat,
    lower: SessionFrame,
    meter: Option<Meter>,
    phase: f64,
) -> Result<Beat, Missing> {
    let forward = with_phase(under, meter, phase).ok_or_else(|| outside(owner))?;
    let earlier = Beat::new(f64::from(forward) - bar_length(meter)).map_err(|_| outside(owner))?;
    let rounds_onto_lower = match owner.position_at(MapPoint::new(owner.stamp(), earlier)) {
        BeatGridQuery::Resolved(position) => {
            matches!(*position.value().value(), MapPosition::Session(frame) if frame >= lower)
        }
        _ => false,
    };
    Ok(if rounds_onto_lower { earlier } else { forward })
}

/// Carries a prepared `alignment` onto the successor `owner` grid.
///
/// The member and group beats that sound together stay the same; only the
/// session frame at which the group reaches its beat moves. `None` when that
/// frame leaves the launch `window` the preparation was asked for.
pub(super) fn carry(
    owner: &BeatGridSnapshot,
    member: &BeatGridSnapshot,
    alignment: BeatAlignment,
    window: &Range<SessionFrame>,
) -> Result<Option<Placement>, Missing> {
    let owner_beat = *alignment.target().value();
    let activation = session_frame(owner, owner_beat, MapPosition::Session(window.start))?;
    if !window.contains(&activation) {
        return Ok(None);
    }
    Ok(Some(Placement {
        alignment: BeatAlignment::new(
            MapPoint::new(member.stamp(), *alignment.source().value()),
            MapPoint::new(owner.stamp(), owner_beat),
        ),
        activation,
    }))
}

/// Continues a sounding `member` from `activation` on, where its applied
/// `plan` carries it.
///
/// The recording frame the plan reaches at the activation and the group beat
/// playing there sound together, so the member neither jumps nor drops out
/// of phase while the successor map takes over.
pub(super) fn continue_on(
    owner: &BeatGridSnapshot,
    member: &BeatGridSnapshot,
    plan: &WarpPlan,
    activation: SessionFrame,
) -> Result<Placement, Missing> {
    let at = MapPosition::Session(activation);
    let source = MapPosition::Asset(resolve(member, plan.source_at(activation), at)?);
    let member_beat = resolve(
        member,
        member.beat_at(MapPoint::new(member.stamp(), source)),
        source,
    )?;
    let owner_beat = owner_beat_at(owner, at)?;
    Ok(Placement {
        alignment: BeatAlignment::new(
            MapPoint::new(member.stamp(), *member_beat.value().value()),
            MapPoint::new(owner.stamp(), owner_beat),
        ),
        activation,
    })
}

/// Gives a sounding replacement its preparation lead and selects the next
/// owner beat without changing the source motion of its applied map.
pub(super) fn retarget_boundary(
    owner: &BeatGridSnapshot,
    commit: SessionFrame,
    frontier: SessionFrame,
) -> Result<SessionFrame, Missing> {
    let earliest = i64::from(commit.max(frontier))
        .checked_add(2_048)
        .map(SessionFrame::new)
        .ok_or_else(|| Missing::Refused(outside(owner)))?;
    let at = MapPosition::Session(earliest);
    let under = owner_beat_at(owner, at)?;
    let beat = first_boundary(owner, under, earliest, None, 0.0)?;
    let activation = session_frame(owner, beat, at)?;
    if activation < earliest {
        return Err(Missing::Refused(outside(owner)));
    }
    Ok(activation)
}

/// Freezes `placement` as map revision `revision` and its activation plan.
pub(super) fn project(
    owner: &BeatGridSnapshot,
    member: &BeatGridSnapshot,
    placement: Placement,
    revision: WarpMapRevision,
) -> Result<(BeatAlignment, WarpPlan), Missing> {
    let map = WarpMap::projected(member.clone(), owner.clone(), placement.alignment, revision)?;
    let at = MapPosition::Session(placement.activation);
    match WarpPlan::new(map, placement.activation) {
        Ok(plan) => Ok((placement.alignment, plan)),
        Err(WarpPlanError::Source(query)) => {
            resolve(member, query, at).and_then(|_| Err(outside(member).into()))
        }
        Err(WarpPlanError::Rate(query)) => {
            resolve(member, query, at).and_then(|_| Err(outside(member).into()))
        }
        Err(_) => Err(outside(member).into()),
    }
}

/// Carries a grid refusal out as the reason a placement cannot be made.
///
/// `at` names the coverage a grid without any geometry would have to publish.
fn resolve<T>(
    grid: &BeatGridSnapshot,
    query: BeatGridQuery<T>,
    at: MapPosition,
) -> Result<T, Missing> {
    match query {
        BeatGridQuery::Resolved(value) => Ok(value),
        BeatGridQuery::Uncovered { required } => Err(Missing::Coverage(required)),
        BeatGridQuery::Unavailable(BeatGridUnavailable::NoGeometry) => {
            Err(Missing::Coverage(MapRegion::point(at)))
        }
        BeatGridQuery::Stale { expected, given } => {
            Err(Missing::Refused(SyncError::StaleGridRevision {
                current: expected,
                given,
            }))
        }
        _ => Err(Missing::Refused(outside(grid))),
    }
}

fn outside(grid: &BeatGridSnapshot) -> SyncError {
    SyncError::OutsideGrid { grid_id: grid.id() }
}

fn owner_beat_at(owner: &BeatGridSnapshot, position: MapPosition) -> Result<Beat, Missing> {
    let beat = resolve(
        owner,
        owner.beat_at(MapPoint::new(owner.stamp(), position)),
        position,
    )?;
    Ok(*beat.value().value())
}

fn member_beat_at_or_next(
    member: &BeatGridSnapshot,
    position: MapPosition,
) -> Result<Beat, Missing> {
    let query = member.beat_at_or_next(MapPoint::new(member.stamp(), position));
    let beat = resolve(member, query, position)?;
    Ok(*beat.value().value())
}

/// The bar a member proves at `beat`; `None` when it proves only beats.
fn bar_of(
    member: &BeatGridSnapshot,
    beat: Beat,
    at: MapPosition,
) -> Result<Option<Meter>, Missing> {
    match member.meter_at(MapPoint::new(member.stamp(), beat)) {
        BeatGridQuery::Resolved(meter) if meter.evidence() != BeatEvidence::Extrapolated => {
            Ok(Some(*meter.value()))
        }
        BeatGridQuery::Resolved(_) | BeatGridQuery::Unavailable(BeatGridUnavailable::NoMeter) => {
            Ok(None)
        }
        refusal => resolve(member, refusal, at).map(|_| None),
    }
}

/// The owner bar a member with `member_meter` enters in.
///
/// An owner that publishes no meter counts bars of the member's length from
/// its session origin.
fn owner_bar(
    owner: &BeatGridSnapshot,
    beat: Beat,
    member_meter: Option<Meter>,
    at: MapPosition,
) -> Result<Option<Meter>, Missing> {
    let Some(member_meter) = member_meter else {
        return Ok(None);
    };
    match owner.meter_at(MapPoint::new(owner.stamp(), beat)) {
        BeatGridQuery::Resolved(meter) => Ok(Some(*meter.value())),
        BeatGridQuery::Unavailable(BeatGridUnavailable::NoMeter) => {
            Ok(Some(member_meter.with_downbeat(BeatOrdinal::new(0))))
        }
        refusal => resolve(owner, refusal, at).map(|_| None),
    }
}

fn session_frame(
    owner: &BeatGridSnapshot,
    beat: Beat,
    at: MapPosition,
) -> Result<SessionFrame, Missing> {
    let position = resolve(
        owner,
        owner.position_at(MapPoint::new(owner.stamp(), beat)),
        at,
    )?;
    let MapPosition::Session(frame) = *position.value().value() else {
        return Err(Missing::Refused(outside(owner)));
    };
    Ok(frame)
}

fn asset_frame(member: &BeatGridSnapshot, frame: u64) -> Result<AssetFrame, Missing> {
    frame
        .to_f64()
        .and_then(|frame| AssetFrame::new(frame).ok())
        .ok_or_else(|| Missing::Refused(outside(member)))
}

/// The recording frame an unmapped audible member reaches at `activation`.
///
/// The live stream advances `speed` recording seconds per session second, so
/// the span crosses the resampler once.
fn live_source(
    owner: &BeatGridSnapshot,
    member: &BeatGridSnapshot,
    frontier: PresentationFrontier,
    speed: f64,
    activation: SessionFrame,
) -> Result<AssetFrame, Missing> {
    let span = i64::from(activation)
        .checked_sub(i64::from(frontier.output()))
        .and_then(|span| span.to_f64())
        .ok_or_else(|| Missing::Refused(outside(owner)))?;
    let ratio =
        f64::from(member.axis().sample_rate().get()) / f64::from(owner.axis().sample_rate().get());
    let advanced = frontier
        .source()
        .to_f64()
        .map(|source| source + (span * speed * ratio).ceil())
        .ok_or_else(|| Missing::Refused(outside(member)))?;
    AssetFrame::new(advanced).map_err(|_| Missing::Refused(outside(member)))
}

/// Carries the actual presented source through the applied map's integrated
/// motion, including both a tempo ramp and any measured presentation offset.
fn mapped_live_source(
    member: &BeatGridSnapshot,
    frontier: PresentationFrontier,
    plan: &WarpPlan,
    activation: SessionFrame,
) -> Result<AssetFrame, Missing> {
    let at_frontier = resolve(
        member,
        plan.source_at(frontier.output()),
        MapPosition::Session(frontier.output()),
    )?;
    let at_activation = resolve(
        member,
        plan.source_at(activation),
        MapPosition::Session(activation),
    )?;
    let actual = asset_frame(member, frontier.source())?;
    AssetFrame::new(f64::from(actual) + f64::from(at_activation) - f64::from(at_frontier))
        .map_err(|_| Missing::Refused(outside(member)))
}

fn whole_beat(grid: &BeatGridSnapshot, beat: Beat) -> Result<Beat, Missing> {
    Beat::new(f64::from(beat).ceil()).map_err(|_| Missing::Refused(outside(grid)))
}

/// The first downbeat at or after the whole `beat`; every beat is one when
/// the grid proves no bar.
fn next_downbeat(
    grid: &BeatGridSnapshot,
    beat: Beat,
    meter: Option<Meter>,
) -> Result<Beat, Missing> {
    let Some(meter) = meter else {
        return Ok(beat);
    };
    let ordinal = f64::from(beat)
        .to_i64()
        .ok_or_else(|| Missing::Refused(outside(grid)))?;
    let distance =
        (i64::from(meter.downbeat()) - ordinal).rem_euclid(i64::from(meter.beats_per_bar()));
    ordinal
        .checked_add(distance)
        .and_then(|ordinal| Beat::try_from(BeatOrdinal::new(ordinal)).ok())
        .ok_or_else(|| Missing::Refused(outside(grid)))
}

/// The first beat at or after `beat` whose bar phase is `phase`.
///
/// A grid without a proven bar counts every beat as its downbeat, so only the
/// fractional beat phase carries over.
fn with_phase(beat: Beat, meter: Option<Meter>, phase: f64) -> Option<Beat> {
    let current = bar_phase(beat, meter)?;
    Beat::new(f64::from(beat) + (phase - current).rem_euclid(bar_length(meter))).ok()
}

fn bar_length(meter: Option<Meter>) -> f64 {
    meter.map_or(1.0, |meter| f64::from(meter.beats_per_bar()))
}

fn bar_phase(beat: Beat, meter: Option<Meter>) -> Option<f64> {
    let Some(meter) = meter else {
        return Some(f64::from(beat).rem_euclid(1.0));
    };
    let downbeat = Beat::try_from(meter.downbeat()).ok()?;
    Some((f64::from(beat) - f64::from(downbeat)).rem_euclid(f64::from(meter.beats_per_bar())))
}

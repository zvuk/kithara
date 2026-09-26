use std::ops::Range;

use kithara_signal::{SessionEpoch, SessionFrame, TransportRevision};
use kithara_test_utils::kithara;
use kithara_warp::{
    AssetAxis, AssetExtent, AssetFrame, BeatAlignment, BeatEvidence, BeatGridId, BeatGridQuery,
    BeatGridRevision, BeatGridSnapshot, BeatGridState, BeatMarker, BeatOrdinal, CoordinateError,
    FrameUncertainty, MapAxis, MapPosition, MapSegment, Meter, MeterFacts, PresentationFrontier,
    SegmentFacts, SegmentSet, SessionAxis, WarpMapRevision, WarpPlan,
};

use super::{
    Accept, TestGrid,
    modes::{
        Group, anchor_at_rate, group_in, parent_id, parent_stamp, parent_update, rate, synced_deck,
        synced_deck_at,
    },
};
use crate::{
    AlignmentSource, LoadGeneration, SessionAxisUpdate, SyncAdmission, SyncCapability, SyncEffect,
    SyncError, SyncGroup, SyncMember, SyncMemberKind, SyncMode, SyncOperation, SyncStatusSnapshot,
    SyncTransition, TopologyOperation, consts,
};

/// Beats every `beat_frames` from `first_beat_frame` through `covered`, on a
/// recording `extent` frames long.
pub(super) fn asset_segments_from(
    extent: u64,
    covered: u64,
    beat_frames: u64,
    first_beat_frame: u64,
    meter: Option<MeterFacts>,
) -> SegmentSet {
    let exact = FrameUncertainty::new(0.0).expect("zero uncertainty is finite");
    let marker = |ordinal: u64, frame: u64| {
        BeatMarker::new(
            MapPosition::Asset(AssetFrame::new(frame as f64).expect("fixture frame is finite")),
            Some(BeatOrdinal::new(ordinal as i64)),
            BeatEvidence::Observed,
            exact,
        )
    };
    let segments = (0..(covered - first_beat_frame) / beat_frames)
        .map(|beat| {
            MapSegment::new(
                marker(beat, first_beat_frame + beat * beat_frames),
                marker(beat + 1, first_beat_frame + (beat + 1) * beat_frames),
                SegmentFacts::new(BeatEvidence::Observed, exact, meter),
            )
            .expect("fixture segment advances on both axes")
        })
        .collect();
    SegmentSet::new(
        MapAxis::Asset(AssetAxis::new(rate(48_000), AssetExtent::Bounded(extent))),
        segments,
    )
    .expect("fixture segment set is contiguous")
}

fn asset_segments(frames: u64, beat_frames: u64, meter: Option<MeterFacts>) -> SegmentSet {
    asset_segments_from(frames, frames, beat_frames, 0, meter)
}

pub(super) fn complete(id: BeatGridId, segments: SegmentSet) -> BeatGridSnapshot {
    BeatGridSnapshot::segments(
        id,
        BeatGridRevision::first(),
        BeatGridState::Complete,
        segments,
    )
    .expect("fixture asset grid is valid")
}

fn asset_grid_with_meter(
    id: BeatGridId,
    frames: u64,
    beat_frames: u64,
    meter: Option<MeterFacts>,
) -> BeatGridSnapshot {
    complete(id, asset_segments(frames, beat_frames, meter))
}

pub(super) fn asset_grid(id: BeatGridId, frames: u64, beat_frames: u64) -> BeatGridSnapshot {
    asset_grid_with_meter(id, frames, beat_frames, None)
}

pub(super) fn observed(meter: Meter) -> Option<MeterFacts> {
    let exact = FrameUncertainty::new(0.0).expect("zero uncertainty is finite");
    Some(MeterFacts::new(meter, BeatEvidence::Observed, exact))
}

pub(super) fn four_four() -> Meter {
    Meter::new(4).expect("fixture meter is valid")
}

/// A four-four track grid, so bar phase is observable in the entry window.
pub(super) fn four_four_grid(id: BeatGridId, frames: u64, beat_frames: u64) -> BeatGridSnapshot {
    asset_grid_with_meter(id, frames, beat_frames, observed(four_four()))
}

/// Attaches `grid` to `group` and returns what the new topology fence
/// withdrew.
pub(super) fn attach_grid(group: &mut Group, grid: BeatGridSnapshot) -> SyncTransition {
    let base = group.topology().expect("topology").stamp();
    let admission = group
        .transact(SyncOperation::Topology {
            base,
            operations: Box::new([TopologyOperation::Attach {
                member: SyncMember::Grid {
                    alignment: None,
                    grid: Box::new(TestGrid(grid)),
                },
            }]),
        })
        .expect("a deck admits a track grid");
    let SyncAdmission::TopologyChanged { transition, .. } = admission else {
        panic!("expected a topology change, got {admission:?}");
    };
    transition
}

pub(super) fn window(earliest: i64, end: i64) -> Range<SessionFrame> {
    SessionFrame::new(earliest)..SessionFrame::new(end)
}

pub(super) fn cue(frame: u64) -> AlignmentSource {
    AlignmentSource::Prepared(AssetFrame::new(frame as f64).expect("fixture cue is finite"))
}

pub(super) fn frontier(source: u64, output: i64) -> AlignmentSource {
    frontier_at_speed(source, output, 1.0)
}

fn frontier_at_speed(source: u64, output: i64, speed: f64) -> AlignmentSource {
    AlignmentSource::Audible {
        frontier: PresentationFrontier::builder()
            .source(source)
            .output(SessionFrame::new(output))
            .build(),
        speed,
    }
}

pub(super) fn prepare_in(
    group: &mut Group,
    target: BeatGridId,
    source: AlignmentSource,
    window: Range<SessionFrame>,
) -> Result<SyncAdmission, SyncError> {
    group
        .transact(SyncOperation::Prepare {
            target,
            load: LoadGeneration::first(),
            transport: TransportRevision::first(),
            source,
            window,
        })
        .map_err(|rejected| rejected.error().clone())
}

pub(super) fn prepare(
    group: &mut Group,
    target: BeatGridId,
    source: AlignmentSource,
    earliest: i64,
) -> SyncAdmission {
    prepare_in(group, target, source, window(earliest, consts::OPEN_END))
        .expect("the preparation is admitted")
}

pub(super) fn projection(admission: &SyncAdmission) -> (BeatAlignment, &WarpPlan) {
    let SyncAdmission::Prepared(preparation) = admission else {
        panic!("expected a prepared member, got {admission:?}");
    };
    let SyncEffect::Projection {
        alignment, plan, ..
    } = preparation.effect()
    else {
        panic!("expected a projection, got {preparation:?}");
    };
    (*alignment, plan)
}

pub(super) fn source_and_activation(admission: &SyncAdmission) -> (u64, SessionFrame) {
    let (_, plan) = projection(admission);
    (plan.activation().source(), plan.activation().output())
}

#[kithara::test]
fn preparation_carries_the_next_source_beat_to_the_next_deck_beat() {
    let mut group = synced_deck();
    let track = BeatGridId::allocate().expect("grid id");
    attach_grid(&mut group, asset_grid(track, 480_000, 24_000));

    let admission = prepare(&mut group, track, cue(10_000), 5_000);

    assert!(matches!(admission, SyncAdmission::Prepared { .. }));
    assert_eq!(
        source_and_activation(&admission),
        (24_000, SessionFrame::new(24_000))
    );
}

#[kithara::test]
fn preparation_before_the_first_grid_beat_cues_that_first_beat() {
    let mut group = synced_deck();
    let track = BeatGridId::allocate().expect("grid id");
    attach_grid(
        &mut group,
        complete(
            track,
            asset_segments_from(480_000, 480_000, 30_000, 30_000, None),
        ),
    );

    let admission = prepare(&mut group, track, cue(6_000), 6_000);

    assert!(
        matches!(admission, SyncAdmission::Prepared { .. }),
        "{admission:?}"
    );
    assert_eq!(source_and_activation(&admission).0, 30_000);
}

#[kithara::test]
fn audible_exact_beat_selects_a_reachable_future_cue() {
    let mut group = synced_deck();
    let track = BeatGridId::allocate().expect("grid id");
    attach_grid(&mut group, asset_grid(track, 480_000, 24_000));

    let admission = prepare(&mut group, track, frontier(0, 0), 0);

    assert_eq!(
        source_and_activation(&admission),
        (24_000, SessionFrame::new(24_000))
    );
}

#[kithara::test]
fn audible_manual_speed_selects_the_cue_the_faster_stream_reaches() {
    let mut group = synced_deck();
    let track = BeatGridId::allocate().expect("grid id");
    attach_grid(&mut group, asset_grid(track, 480_000, 24_000));

    let admission = prepare(&mut group, track, frontier_at_speed(0, 0, 2.0), 0);

    // At twice the recording speed the stream reaches source 48_000 by the
    // first deck beat, so the cue lies there rather than behind it.
    assert_eq!(
        source_and_activation(&admission),
        (48_000, SessionFrame::new(24_000))
    );
}

#[kithara::test]
fn an_audible_member_without_a_forward_speed_is_refused() {
    let mut group = synced_deck();
    let track = BeatGridId::allocate().expect("grid id");
    attach_grid(&mut group, asset_grid(track, 480_000, 24_000));

    for speed in [0.0, -1.0, f64::NAN] {
        assert_eq!(
            prepare_in(
                &mut group,
                track,
                frontier_at_speed(0, 0, speed),
                window(0, consts::OPEN_END)
            ),
            Err(SyncError::Coordinate(CoordinateError::NonInvertibleRate)),
            "{speed}"
        );
    }
    assert!(group.pending.is_empty());
}

#[kithara::test]
fn audible_alignment_seeks_ahead_of_the_live_source_at_the_next_host_downbeat() {
    let mut group = synced_deck();
    let track = BeatGridId::allocate().expect("grid id");
    attach_grid(&mut group, four_four_grid(track, 960_000, 24_000));

    let admission = prepare(&mut group, track, frontier(383_872, 60_768), 61_216);

    // Preparation reaches output 61_216 (beat 2.55), so the next Host downbeat
    // is beat 4 at 96_000. The live mapping stands at source 419_104 there
    // (beat 17.46); the next source downbeat is beat 20 at 480_000, which the
    // old stream has not reached when the seek activates.
    assert_eq!(
        source_and_activation(&admission),
        (480_000, SessionFrame::new(96_000))
    );
}

#[kithara::test]
#[case::source_has_farther_to_travel(6_000, 18_000, 18_000, 6_000)]
#[case::host_has_farther_to_travel(18_000, 6_000, 6_000, 18_000)]
fn preparation_preserves_both_phase_error_directions(
    #[case] source_frontier: u64,
    #[case] output_frontier: i64,
    #[case] expected_source_distance: u64,
    #[case] expected_output_distance: i64,
) {
    let mut group = synced_deck();
    let track = BeatGridId::allocate().expect("grid id");
    attach_grid(&mut group, asset_grid(track, 480_000, 24_000));

    let admission = prepare(&mut group, track, cue(source_frontier), output_frontier);
    let (source, activation) = source_and_activation(&admission);

    assert_eq!(source, 24_000);
    assert_eq!(activation, SessionFrame::new(24_000));
    assert_eq!(source - source_frontier, expected_source_distance);
    assert_eq!(
        i64::from(activation) - output_frontier,
        expected_output_distance
    );
}

#[kithara::test]
fn different_bpm_grids_produce_one_coherent_phase_and_rate_decision() {
    let mut group = synced_deck();
    let track = BeatGridId::allocate().expect("grid id");
    attach_grid(
        &mut group,
        complete(track, asset_segments(480_000, 30_000, None)),
    );

    let admission = prepare(&mut group, track, cue(6_000), 6_000);
    let (source, activation) = source_and_activation(&admission);
    assert_eq!(source, 30_000);
    assert_eq!(activation, SessionFrame::new(24_000));
    let (alignment, plan) = projection(&admission);
    assert_eq!(f64::from(*alignment.target().value()), 1.0);

    let BeatGridQuery::Resolved(rate) = plan.rate_at(activation) else {
        panic!("the prepared projection answers a rate at the activation it was built for");
    };
    assert!((rate - 1.25).abs() < 1e-9, "{rate}");
}

#[kithara::test]
fn preparation_aligns_a_known_track_downbeat_to_the_session_origin_phase() {
    let mut group = synced_deck();
    let track = BeatGridId::allocate().expect("grid id");
    attach_grid(&mut group, four_four_grid(track, 480_000, 24_000));

    let admission = prepare(&mut group, track, cue(100_000), 100_000);

    assert_eq!(
        source_and_activation(&admission),
        (192_000, SessionFrame::new(192_000))
    );
}

#[kithara::test]
fn preparation_preserves_non_four_four_downbeat_phase() {
    let mut group = synced_deck();
    let track = BeatGridId::allocate().expect("grid id");
    let meter = Meter::new(3)
        .expect("fixture meter is valid")
        .with_downbeat(BeatOrdinal::new(1));
    attach_grid(
        &mut group,
        asset_grid_with_meter(track, 480_000, 24_000, observed(meter)),
    );

    let admission = prepare(&mut group, track, cue(50_000), 50_000);
    let (source, activation) = source_and_activation(&admission);

    assert_eq!(source, 96_000, "source beat 4 is the next 3/4 downbeat");
    assert_eq!(
        activation,
        SessionFrame::new(72_000),
        "Host beat 3 is the nearest 3/4 downbeat"
    );
}

#[kithara::test]
fn preparation_keeps_the_nearest_host_downbeat_for_a_large_source_jump() {
    let mut group = synced_deck();
    let track = BeatGridId::allocate().expect("grid id");
    attach_grid(&mut group, four_four_grid(track, 480_000, 24_000));

    let admission = prepare(&mut group, track, cue(252_000), 191_999);

    assert_eq!(
        source_and_activation(&admission),
        (288_000, SessionFrame::new(192_000))
    );
}

#[kithara::test]
fn host_seek_quantizes_between_beats_and_keeps_an_exact_beat() {
    let mut group = synced_deck();
    let track = BeatGridId::allocate().expect("grid id");
    attach_grid(&mut group, asset_grid(track, 480_000, 24_000));

    let between = prepare(&mut group, track, cue(10_000), 10_000);
    let (source, activation) = source_and_activation(&between);
    assert_eq!(source, 24_000);
    assert!(activation >= SessionFrame::new(10_000));
    let on_beat = prepare(&mut group, track, cue(24_000), 10_000);
    assert_eq!(source_and_activation(&on_beat).0, 24_000);
}

#[kithara::test]
fn each_member_keeps_its_own_prepared_map() {
    let mut group = synced_deck();
    let first = BeatGridId::allocate().expect("grid id");
    let second = BeatGridId::allocate().expect("grid id");
    attach_grid(&mut group, asset_grid(first, 480_000, 24_000));
    attach_grid(&mut group, asset_grid(second, 480_000, 36_000));

    let first_admission = prepare(&mut group, first, cue(0), 0);
    let second_admission = prepare(&mut group, second, cue(0), 0);

    let stamp = |admission: &SyncAdmission| match admission {
        SyncAdmission::Prepared(preparation) => preparation.stamp(),
        other => panic!("expected a prepared member, got {other:?}"),
    };
    assert_eq!(stamp(&first_admission).member().grid_id(), first);
    assert_eq!(stamp(&second_admission).member().grid_id(), second);
    assert_ne!(
        stamp(&first_admission).operation(),
        stamp(&second_admission).operation()
    );
    assert_ne!(
        projection(&first_admission).1.activation().revision(),
        projection(&second_admission).1.activation().revision(),
        "every preparation mints its own map revision"
    );
    assert_eq!(group.pending.len(), 2, "both preparations stay pending");
}

#[kithara::test]
fn a_route_boundary_drops_the_preparation_planned_on_the_previous_axis() {
    let mut group = group_in(SyncMode::HostSync, SyncMemberKind::Grid);
    group
        .accept_parent(parent_update(
            parent_stamp(parent_id(), 1),
            anchor_at_rate(2.0, 44_100),
        ))
        .expect("the first anchor makes the deck grid live");
    let track = BeatGridId::allocate().expect("grid id");
    attach_grid(&mut group, asset_grid(track, 480_000, 24_000));
    let admission = prepare(&mut group, track, cue(0), 0);
    assert!(matches!(admission, SyncAdmission::Prepared { .. }));

    group
        .accept_axis(SessionAxisUpdate::new(SessionAxis::new(
            rate(48_000),
            SessionEpoch::new(1),
        )))
        .expect("the successor epoch steps the deck through an unavailable grid");

    assert!(group.pending.is_empty());
}

#[kithara::test]
fn a_track_without_geometry_waits_for_its_grid() {
    let mut group = synced_deck();
    let track = BeatGridId::allocate().expect("grid id");
    attach_grid(
        &mut group,
        BeatGridSnapshot::unavailable(
            track,
            BeatGridRevision::first(),
            MapAxis::Asset(AssetAxis::new(rate(48_000), AssetExtent::Bounded(480_000))),
        ),
    );

    let admission = prepare(&mut group, track, cue(0), 0);

    assert!(
        matches!(admission, SyncAdmission::Deferred { .. }),
        "{admission:?}"
    );
    assert!(
        matches!(group.status(), SyncStatusSnapshot::WaitingForGrid { .. }),
        "{:?}",
        group.status()
    );
}

#[kithara::test]
fn the_launch_window_admits_its_last_frame_and_refuses_its_end() {
    let mut group = synced_deck();
    let track = BeatGridId::allocate().expect("grid id");
    attach_grid(&mut group, four_four_grid(track, 480_000, 24_000));
    let status = group.status();
    let next = group.next_operation;

    let refused = prepare_in(&mut group, track, cue(0), window(100_000, 192_000));
    assert_eq!(
        refused,
        Err(SyncError::NoAdmissibleBoundary {
            member_id: track,
            first: SessionFrame::new(192_000),
            end: SessionFrame::new(192_000),
        })
    );
    assert!(group.pending.is_empty(), "a refusal prepares nothing");
    assert_eq!(group.status(), status, "a refusal changes no status");
    assert_eq!(group.next_operation, next, "a refusal spends no operation");

    let admitted = prepare_in(&mut group, track, cue(0), window(100_000, 192_001))
        .expect("the last frame before the end is admissible");
    assert_eq!(
        source_and_activation(&admitted),
        (0, SessionFrame::new(192_000))
    );
    let SyncAdmission::Prepared(preparation) = &admitted else {
        panic!("expected a prepared member, got {admitted:?}");
    };
    assert_eq!(Some(preparation.stamp().operation()), next);
    assert!(matches!(
        group.status(),
        SyncStatusSnapshot::Prepared { activation, .. } if activation == SessionFrame::new(192_000)
    ));
}

#[kithara::test]
fn the_launch_window_starts_at_the_first_frame_the_caller_reaches() {
    let mut group = synced_deck();
    let track = BeatGridId::allocate().expect("grid id");
    attach_grid(&mut group, asset_grid(track, 480_000, 24_000));

    let on_beat = prepare(&mut group, track, cue(0), 48_000);
    let past_beat = prepare(&mut group, track, cue(0), 48_001);

    assert_eq!(source_and_activation(&on_beat).1, SessionFrame::new(48_000));
    assert_eq!(
        source_and_activation(&past_beat).1,
        SessionFrame::new(72_000)
    );
    assert_eq!(
        group.pending.len(),
        1,
        "a member holds one pending decision"
    );
}

#[kithara::test]
fn an_off_deck_prepares_no_member() {
    let mut group = group_in(SyncMode::Off, SyncMemberKind::Grid);
    let track = BeatGridId::allocate().expect("grid id");
    attach_grid(&mut group, asset_grid(track, 480_000, 24_000));

    assert_eq!(
        prepare_in(&mut group, track, cue(0), window(0, consts::OPEN_END)),
        Err(SyncError::CapabilityUnavailable {
            capability: SyncCapability::Alignment,
        })
    );
}

#[kithara::test]
fn an_audible_member_under_a_map_the_group_never_applied_is_refused() {
    let mut group = synced_deck();
    let track = BeatGridId::allocate().expect("grid id");
    attach_grid(&mut group, asset_grid(track, 480_000, 24_000));
    let given = WarpMapRevision::first();
    let source = AlignmentSource::Audible {
        frontier: PresentationFrontier::builder()
            .warp_map(given)
            .source(0)
            .output(SessionFrame::new(0))
            .build(),
        speed: 1.0,
    };

    assert_eq!(
        prepare_in(&mut group, track, source, window(0, consts::OPEN_END)),
        Err(SyncError::AudibleMapMismatch {
            member_id: track,
            expected: None,
            given: Some(given),
        })
    );
}

#[kithara::test]
fn detaching_a_member_withdraws_its_preparation() {
    let mut group = synced_deck();
    let track = BeatGridId::allocate().expect("grid id");
    attach_grid(&mut group, asset_grid(track, 480_000, 24_000));
    let _ = prepare(&mut group, track, cue(0), 0);
    let base = group.topology().expect("topology").stamp();

    let _ = group
        .transact(SyncOperation::Topology {
            base,
            operations: Box::new([TopologyOperation::Detach { member: track }]),
        })
        .expect("the deck releases its track");

    assert!(group.pending.is_empty());
    assert!(matches!(group.status(), SyncStatusSnapshot::Off { .. }));
}

#[kithara::test]
fn a_member_absent_from_the_group_is_not_prepared() {
    let mut group = synced_deck();
    let stranger = BeatGridId::allocate().expect("grid id");

    assert!(prepare_in(&mut group, stranger, cue(0), window(0, consts::OPEN_END)).is_err());
    assert!(group.pending.is_empty());
}

pub(super) fn building(id: BeatGridId, segments: SegmentSet) -> BeatGridSnapshot {
    BeatGridSnapshot::segments(
        id,
        BeatGridRevision::first(),
        BeatGridState::Building,
        segments,
    )
    .expect("a building grid is valid")
}

pub(super) fn replace_grid(group: &mut Group, grid: BeatGridSnapshot) {
    let base = group.topology().expect("topology").stamp();
    let _ = group
        .transact(SyncOperation::Topology {
            base,
            operations: Box::new([TopologyOperation::Replace {
                member: grid.id(),
                replacement: SyncMember::Grid {
                    alignment: None,
                    grid: Box::new(TestGrid(grid)),
                },
            }]),
        })
        .expect("a deck replaces its track grid");
}

#[kithara::test]
fn a_boundary_rounded_onto_the_first_window_frame_is_admitted() {
    // At 123 BPM a beat lasts 23_414.63 frames, so beat 1 sounds on frame
    // 23_415 while the beat playing there is already a fraction past 1.
    let mut group = synced_deck_at(123.0);
    let track = BeatGridId::allocate().expect("grid id");
    attach_grid(&mut group, asset_grid(track, 480_000, 24_000));

    let admission = prepare_in(&mut group, track, cue(0), window(23_415, 23_416))
        .expect("the beat rounded onto the window start is admissible");

    let (alignment, plan) = projection(&admission);
    assert_eq!(f64::from(*alignment.target().value()), 1.0);
    assert_eq!(plan.activation().output(), SessionFrame::new(23_415));
}

fn pickup(cue_beat: f64) -> f64 {
    let mut group = synced_deck();
    let track = BeatGridId::allocate().expect("grid id");
    let source_meter = four_four().with_downbeat(BeatOrdinal::new(1));
    attach_grid(
        &mut group,
        asset_grid_with_meter(track, 480_000, 24_000, observed(source_meter)),
    );
    let cue = AssetFrame::new(cue_beat * 24_000.0).expect("fixture cue is finite");

    // The deck frontier stands on its beat 1, the first eligible host beat.
    let admission = prepare(&mut group, track, AlignmentSource::Cued(cue), 24_000);

    let (alignment, _) = projection(&admission);
    assert_eq!(f64::from(*alignment.source().value()), cue_beat);
    f64::from(*alignment.target().value())
}

#[kithara::test]
fn pickup_track_start_keeps_its_weak_beat_phase() {
    assert_eq!(pickup(0.0), 3.0);
}

#[kithara::test]
fn pickup_track_start_preserves_fractional_beat_phase() {
    assert_eq!(pickup(0.5), 3.5);
}

#[kithara::test]
fn a_window_too_short_for_a_downbeat_is_refused() {
    let mut group = synced_deck();
    let track = BeatGridId::allocate().expect("grid id");
    attach_grid(&mut group, four_four_grid(track, 480_000, 24_000));
    let next = group.next_operation;

    let refused = prepare_in(&mut group, track, cue(0), window(194_000, 200_000));

    assert_eq!(
        refused,
        Err(SyncError::NoAdmissibleBoundary {
            member_id: track,
            first: SessionFrame::new(288_000),
            end: SessionFrame::new(200_000),
        })
    );
    assert!(group.pending.is_empty());
    assert_eq!(group.next_operation, next);
}

#[kithara::test]
fn a_building_grid_that_proves_its_bar_is_prepared() {
    let mut group = synced_deck();
    let track = BeatGridId::allocate().expect("grid id");
    attach_grid(
        &mut group,
        building(
            track,
            asset_segments(480_000, 24_000, observed(four_four())),
        ),
    );

    let admission = prepare(&mut group, track, cue(0), 0);

    assert_eq!(source_and_activation(&admission), (0, SessionFrame::new(0)));
}

#[kithara::test]
fn a_building_grid_that_cannot_prove_its_bar_waits_for_it() {
    let mut group = synced_deck();
    let track = BeatGridId::allocate().expect("grid id");
    attach_grid(
        &mut group,
        building(track, asset_segments(480_000, 24_000, None)),
    );

    let admission = prepare(&mut group, track, cue(0), 0);

    assert!(
        matches!(admission, SyncAdmission::Deferred { .. }),
        "{admission:?}"
    );
    assert!(matches!(
        group.status(),
        SyncStatusSnapshot::WaitingForGrid { .. }
    ));
}

#[kithara::test]
fn a_building_track_grid_defers_until_it_covers_the_entry() {
    let mut group = synced_deck();
    let track = BeatGridId::allocate().expect("grid id");
    let meter = observed(four_four());
    attach_grid(
        &mut group,
        building(
            track,
            asset_segments_from(480_000, 192_000, 24_000, 0, meter),
        ),
    );

    let covered = prepare(&mut group, track, cue(48_000), 0);
    assert_eq!(
        source_and_activation(&covered),
        (96_000, SessionFrame::new(0))
    );
    let uncovered = prepare(&mut group, track, cue(200_000), 0);
    assert!(
        matches!(uncovered, SyncAdmission::Deferred { .. }),
        "{uncovered:?}"
    );
    assert!(matches!(
        group.status(),
        SyncStatusSnapshot::WaitingForGrid { .. }
    ));

    replace_grid(&mut group, four_four_grid(track, 480_000, 24_000));
    let complete = prepare(&mut group, track, cue(200_000), 0);

    assert_eq!(
        source_and_activation(&complete),
        (288_000, SessionFrame::new(0))
    );
    assert!(matches!(
        group.status(),
        SyncStatusSnapshot::Prepared { .. }
    ));
}

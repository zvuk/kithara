use kithara_beat::{BeatGridModel, BeatGridState};
use kithara_signal::SessionFrame;
use kithara_test_utils::kithara;
use kithara_warp::SessionBeat;

use super::fixtures::{boundary_at_or_before, grid, host, position};
use crate::{Bound, covers, entry, phase_error, speed};

#[kithara::test]
fn entry_preserves_the_media_offset_between_source_and_host_beats() {
    let host = host(120.0, 4);
    let grid = grid(480_000, 480_000, 24_000, 0, None, BeatGridState::Final);
    let cue = position(10_000);
    let at = entry(
        &host,
        &grid,
        cue,
        Bound::AtOrAfter(SessionFrame::new(5_000)),
    )
    .expect("covered cue has an entry");

    assert_eq!(at, SessionFrame::new(10_000));
    assert_eq!(
        boundary_at_or_before(&grid, position(24_000)),
        Some(position(24_000))
    );
    assert_eq!(
        host.frame_at(SessionBeat::new(1.0).expect("beat")),
        SessionFrame::new(24_000)
    );
    assert_eq!(speed(&host, &grid, at), 1.0);
    assert!(phase_error(&host, &grid, cue, at).shortest().abs() < 1e-9);
}

#[kithara::test]
fn entry_before_the_first_observed_beat_has_no_coverage() {
    let host = host(120.0, 4);
    let grid = grid(480_000, 480_000, 30_000, 30_000, None, BeatGridState::Final);
    let cue = position(6_000);

    assert!(!covers(&grid, cue));
    assert_eq!(boundary_at_or_before(&grid, cue), None);
    assert_eq!(
        entry(
            &host,
            &grid,
            cue,
            Bound::AtOrAfter(SessionFrame::new(6_000))
        ),
        None
    );
    assert_eq!(
        boundary_at_or_before(&grid, position(30_000)),
        Some(position(30_000))
    );
    assert!((f64::from(speed(&host, &grid, SessionFrame::new(6_000))) - 1.25).abs() < 1e-9);
}

#[kithara::test]
#[case::source_has_farther_to_travel(6_000, 18_000, 18_000, 6_000, 30_000)]
#[case::host_has_farther_to_travel(18_000, 6_000, 6_000, 18_000, 18_000)]
fn phase_error_keeps_both_half_beat_directions_and_entry_preserves_the_cue(
    #[case] source_frontier: u64,
    #[case] output_frontier: i64,
    #[case] expected_source_distance: u64,
    #[case] expected_output_distance: i64,
    #[case] expected_entry: i64,
) {
    let host = host(120.0, 4);
    let grid = grid(480_000, 480_000, 24_000, 0, None, BeatGridState::Final);
    let cue = position(source_frontier);
    let at = SessionFrame::new(output_frontier);
    let error = phase_error(&host, &grid, cue, at);

    assert_eq!(
        grid.as_raw().beats[1].at,
        cue.as_secs_f64() + position(expected_source_distance).as_secs_f64()
    );
    assert_eq!(
        i64::from(host.frame_at(SessionBeat::new(1.0).expect("beat"))) - output_frontier,
        expected_output_distance
    );
    assert_eq!(error.seconds, 0.25);
    assert_eq!(error.period, 0.5);
    assert_eq!(error.beat_seconds, 0.5);
    assert_eq!(
        entry(&host, &grid, cue, Bound::AtOrAfter(at)),
        Some(SessionFrame::new(expected_entry))
    );
    assert!(
        phase_error(&host, &grid, cue, SessionFrame::new(expected_entry))
            .shortest()
            .abs()
            < 1e-9
    );
}

#[kithara::test]
fn different_bpms_share_one_phase_preserving_entry_and_speed() {
    let host = host(120.0, 4);
    let grid = grid(480_000, 480_000, 30_000, 0, None, BeatGridState::Final);
    let cue = position(6_000);
    let at = SessionFrame::new(6_000);

    assert!((f64::from(speed(&host, &grid, at)) - 1.25).abs() < 1e-9);
    assert_eq!(phase_error(&host, &grid, cue, at).seconds, 0.59375);
    let activation = entry(&host, &grid, cue, Bound::AtOrAfter(at)).expect("covered cue");
    assert_eq!(activation, SessionFrame::new(28_800));
    assert_eq!(f64::from(host.beat_at(activation)), 1.2);
    assert!(phase_error(&host, &grid, cue, activation).shortest().abs() < 1e-9);
    assert_eq!(
        boundary_at_or_before(&grid, position(30_000)),
        Some(position(30_000))
    );
    assert_eq!(
        host.frame_at(SessionBeat::new(1.0).expect("beat")),
        SessionFrame::new(24_000)
    );
}

#[kithara::test]
fn an_in_phase_metered_cue_is_an_inclusive_entry_on_both_sides() {
    let host = host(120.0, 4);
    let grid = grid(
        480_000,
        480_000,
        24_000,
        0,
        Some((4, 0)),
        BeatGridState::Final,
    );
    let cue = position(100_000);
    let at = SessionFrame::new(100_000);

    assert_eq!(boundary_at_or_before(&grid, cue), Some(position(96_000)));
    assert_eq!(
        boundary_at_or_before(&grid, position(192_000)),
        Some(position(192_000))
    );
    assert_eq!(
        host.frame_at(SessionBeat::new(8.0).expect("beat")),
        SessionFrame::new(192_000)
    );
    assert!(phase_error(&host, &grid, cue, at).shortest().abs() < 1e-9);
    assert_eq!(entry(&host, &grid, cue, Bound::AtOrBefore(at)), Some(at));
    assert_eq!(entry(&host, &grid, cue, Bound::AtOrAfter(at)), Some(at));
}

#[kithara::test]
fn three_four_entries_keep_the_offset_from_the_observed_downbeat() {
    let host = host(120.0, 3);
    let grid = grid(
        480_000,
        480_000,
        24_000,
        0,
        Some((3, 1)),
        BeatGridState::Final,
    );
    let cue = position(50_000);
    let at = SessionFrame::new(50_000);

    assert_eq!(boundary_at_or_before(&grid, cue), Some(position(24_000)));
    assert_eq!(
        boundary_at_or_before(&grid, position(96_000)),
        Some(position(96_000))
    );
    assert_eq!(
        host.frame_at(SessionBeat::new(3.0).expect("beat")),
        SessionFrame::new(72_000)
    );
    assert_eq!(
        entry(&host, &grid, position(96_000), Bound::AtOrAfter(at)),
        Some(SessionFrame::new(72_000))
    );
    let activation = entry(&host, &grid, cue, Bound::AtOrAfter(at)).expect("covered cue");
    assert_eq!(activation, SessionFrame::new(98_000));
    assert_eq!(phase_error(&host, &grid, cue, activation).period, 1.5);
    assert!(phase_error(&host, &grid, cue, activation).shortest().abs() < 1e-9);
}

#[kithara::test]
fn a_large_source_offset_selects_the_nearest_entry_without_moving_the_cue() {
    let host = host(120.0, 4);
    let grid = grid(
        480_000,
        480_000,
        24_000,
        0,
        Some((4, 0)),
        BeatGridState::Final,
    );
    let cue = position(252_000);
    let at = SessionFrame::new(191_999);

    assert_eq!(boundary_at_or_before(&grid, cue), Some(position(192_000)));
    assert_eq!(
        boundary_at_or_before(&grid, position(288_000)),
        Some(position(288_000))
    );
    assert_eq!(
        entry(&host, &grid, position(288_000), Bound::AtOrAfter(at)),
        Some(SessionFrame::new(192_000))
    );
    let activation = entry(&host, &grid, cue, Bound::AtOrAfter(at)).expect("covered cue");
    assert_eq!(activation, SessionFrame::new(252_000));
    assert_eq!(phase_error(&host, &grid, cue, activation).seconds, 0.0);
}

#[kithara::test]
fn entries_preserve_between_beat_cues_and_exact_beats() {
    let host = host(120.0, 4);
    let grid = grid(480_000, 480_000, 24_000, 0, None, BeatGridState::Final);
    let bound = Bound::AtOrAfter(SessionFrame::new(10_000));

    assert_eq!(
        entry(&host, &grid, position(10_000), bound),
        Some(SessionFrame::new(10_000))
    );
    assert_eq!(
        entry(&host, &grid, position(24_000), bound),
        Some(SessionFrame::new(24_000))
    );
    assert_eq!(
        boundary_at_or_before(&grid, position(10_000)),
        Some(position(0))
    );
    assert_eq!(
        boundary_at_or_before(&grid, position(24_000)),
        Some(position(24_000))
    );
}

#[kithara::test]
fn a_boundary_rounded_onto_the_requested_frame_is_an_inclusive_entry() {
    let host = host(123.0, 4);
    let grid = grid(480_000, 480_000, 24_000, 0, None, BeatGridState::Final);
    let at = SessionFrame::new(23_415);

    assert_eq!(host.frame_at(SessionBeat::new(1.0).expect("beat")), at);
    assert_eq!(
        entry(&host, &grid, position(0), Bound::AtOrBefore(at)),
        Some(at)
    );
    assert_eq!(
        entry(&host, &grid, position(0), Bound::AtOrAfter(at)),
        Some(at)
    );
}

#[kithara::test]
fn pickup_entries_keep_the_weak_beat_phase() {
    let host = host(120.0, 4);
    let grid = grid(
        480_000,
        480_000,
        24_000,
        0,
        Some((4, 1)),
        BeatGridState::Final,
    );
    let activation = entry(
        &host,
        &grid,
        position(0),
        Bound::AtOrAfter(SessionFrame::new(24_000)),
    )
    .expect("the observed pickup meter determines its phase");

    assert_eq!(f64::from(host.beat_at(activation)), 3.0);
    assert_eq!(activation, SessionFrame::new(72_000));
}

#[kithara::test]
fn pickup_entries_preserve_fractional_beat_phase() {
    let host = host(120.0, 4);
    let grid = grid(
        480_000,
        480_000,
        24_000,
        0,
        Some((4, 1)),
        BeatGridState::Final,
    );
    let activation = entry(
        &host,
        &grid,
        position(12_000),
        Bound::AtOrAfter(SessionFrame::new(24_000)),
    )
    .expect("the observed pickup meter determines its fractional phase");

    assert_eq!(f64::from(host.beat_at(activation)), 3.5);
    assert_eq!(activation, SessionFrame::new(84_000));
}

#[kithara::test]
fn a_provisional_grid_with_a_proven_bar_covers_its_entry() {
    let host = host(120.0, 4);
    let grid = grid(
        480_000,
        480_000,
        24_000,
        0,
        Some((4, 0)),
        BeatGridState::Provisional,
    );

    assert!(covers(&grid, position(0)));
    assert_eq!(boundary_at_or_before(&grid, position(0)), Some(position(0)));
    assert_eq!(
        entry(
            &host,
            &grid,
            position(0),
            Bound::AtOrAfter(SessionFrame::new(0))
        ),
        Some(SessionFrame::new(0))
    );
}

#[kithara::test]
fn a_provisional_grid_without_meter_uses_beat_phase() {
    let host = host(120.0, 4);
    let grid = grid(
        480_000,
        480_000,
        24_000,
        0,
        None,
        BeatGridState::Provisional,
    );

    assert!(covers(&grid, position(0)));
    assert_eq!(boundary_at_or_before(&grid, position(0)), Some(position(0)));
    assert_eq!(
        entry(
            &host,
            &grid,
            position(0),
            Bound::AtOrAfter(SessionFrame::new(0))
        ),
        Some(SessionFrame::new(0))
    );
    assert_eq!(
        phase_error(&host, &grid, position(0), SessionFrame::new(0)).period,
        0.5
    );
}

#[kithara::test]
fn provisional_coverage_stops_at_the_last_observed_frame() {
    let host = host(120.0, 4);
    let provisional = grid(
        480_000,
        192_000,
        24_000,
        0,
        Some((4, 0)),
        BeatGridState::Provisional,
    );
    let bound = Bound::AtOrAfter(SessionFrame::new(0));

    assert!(covers(&provisional, position(48_000)));
    assert!(covers(&provisional, position(192_000)));
    assert_eq!(
        entry(&host, &provisional, position(48_000), bound),
        Some(SessionFrame::new(48_000))
    );
    assert_eq!(
        entry(&host, &provisional, position(96_000), bound),
        Some(SessionFrame::new(0))
    );
    assert!(!covers(&provisional, position(200_000)));
    assert_eq!(entry(&host, &provisional, position(200_000), bound), None);
    let complete = grid(
        480_000,
        480_000,
        24_000,
        0,
        Some((4, 0)),
        BeatGridState::Final,
    );
    assert!(covers(&complete, position(200_000)));
    assert_eq!(
        entry(&host, &complete, position(200_000), bound),
        Some(SessionFrame::new(8_000))
    );
    assert_eq!(
        entry(&host, &complete, position(288_000), bound),
        Some(SessionFrame::new(0))
    );
}

#[kithara::test]
fn a_grid_without_observed_geometry_has_no_entry() {
    let host = host(120.0, 4);
    let model = grid(
        480_000,
        480_000,
        24_000,
        0,
        None,
        BeatGridState::Provisional,
    );
    let mut raw = model.as_raw().clone();
    raw.beats.clear();
    raw.downbeats.clear();
    let grid = BeatGridModel::try_from(raw).expect("unavailable geometry is valid");

    assert!(!covers(&grid, position(0)));
    assert_eq!(boundary_at_or_before(&grid, position(0)), None);
    assert_eq!(
        entry(
            &host,
            &grid,
            position(0),
            Bound::AtOrAfter(SessionFrame::new(0))
        ),
        None
    );
}

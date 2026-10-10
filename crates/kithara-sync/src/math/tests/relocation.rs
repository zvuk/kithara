use kithara_beat::BeatGridState;
use kithara_signal::SessionFrame;
use kithara_test_utils::kithara;

use super::fixtures::{grid, host, position};
use crate::{Bound, covers, entry, phase_error};

#[kithara::test]
fn relocation_entry_keeps_the_exact_cue_and_the_requested_lower_bound() {
    let host = host(120.0, 4);
    let grid = grid(960_000, 960_000, 24_000, 0, None, BeatGridState::Final);
    let cue = position(36_000);
    let lower = SessionFrame::new(96_000);
    let activation = entry(&host, &grid, cue, Bound::AtOrAfter(lower)).expect("covered cue");

    assert_eq!(activation, SessionFrame::new(108_000));
    assert!(activation >= lower);
    assert_eq!(phase_error(&host, &grid, cue, activation).seconds, 0.0);
}

#[kithara::test]
fn relocation_entry_respects_the_frame_after_the_audible_frontier() {
    let host = host(120.0, 4);
    let grid = grid(960_000, 960_000, 24_000, 0, None, BeatGridState::Final);
    let frontier = SessionFrame::new(96_000);
    let lower = SessionFrame::new(96_001);
    let activation = entry(&host, &grid, position(0), Bound::AtOrAfter(lower))
        .expect("covered cue has a later phase match");

    assert!(activation > frontier);
    assert!(activation >= lower);
    assert_eq!(activation, SessionFrame::new(120_000));
}

#[kithara::test]
fn an_uncovered_relocation_cue_has_no_entry() {
    let host = host(120.0, 4);
    let grid = grid(
        480_000,
        192_000,
        24_000,
        0,
        Some((4, 0)),
        BeatGridState::Provisional,
    );
    let cue = position(300_000);

    assert!(!covers(&grid, cue));
    assert_eq!(
        entry(
            &host,
            &grid,
            cue,
            Bound::AtOrAfter(SessionFrame::new(96_000))
        ),
        None
    );
}

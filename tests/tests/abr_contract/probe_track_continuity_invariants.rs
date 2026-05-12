//! CRITICAL: USER-FACING BUG REPRODUCERS.
//! These 3 bugs exist in main; refactor MUST close all 5 scenarios GREEN.
//!
//! Spec: `.docs/plans/2026-05-11-abr-pull-driven-simplification.md#file-15`
//!
//! Plan 00 skeleton — bodies panic via `unimplemented!()`. Plan 10 fills
//! the bug reproducers (Bug #1 variant-replay, Bug #2 mid-segment drop,
//! Bug #3 premature EOF).

use kithara_platform::time::Duration;
use kithara_test_utils::kithara;

// === Bug #1 — variant switch replays from start ===

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(45)))]
#[serial_test::serial]
async fn variant_switch_does_not_reset_playhead_to_seg_0_bug1() {
    unimplemented!("Plan 10 — Bug #1: variant_switch_does_not_reset_playhead_to_seg_0 reproducer");
}

// === Bug #2 — mid-segment audio drop / segment skip ===

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(45)))]
#[serial_test::serial]
async fn no_audio_gaps_mid_segment_during_steady_playback_bug2() {
    unimplemented!("Plan 10 — Bug #2: no_audio_gaps_mid_segment_during_steady_playback reproducer");
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(45)))]
#[serial_test::serial]
async fn no_segment_skip_during_variant_switch_bug2() {
    unimplemented!("Plan 10 — Bug #2: no_segment_skip_during_variant_switch reproducer");
}

// === Bug #3 — premature EOF ===

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(60)))]
#[serial_test::serial]
async fn no_premature_eof_full_track_to_natural_end_bug3() {
    unimplemented!("Plan 10 — Bug #3: no_premature_eof_full_track_to_natural_end reproducer");
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(60)))]
#[serial_test::serial]
async fn no_premature_eof_after_variant_switch_bug3() {
    unimplemented!("Plan 10 — Bug #3: no_premature_eof_after_variant_switch reproducer");
}

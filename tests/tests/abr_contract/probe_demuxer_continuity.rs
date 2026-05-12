//! Demuxer continuity — Source::read_at offsets monotonic; demuxer next_frame
//! count ≥ 50% expected.
//!
//! Spec: `.docs/plans/2026-05-11-abr-pull-driven-simplification.md#file-14`
//!
//! Plan 00 skeleton — bodies panic via `unimplemented!()`. Replaces T12+T13.
//! Plan 10 fills the demuxer-continuity scenarios.

use kithara_platform::time::Duration;
use kithara_test_utils::kithara;

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn read_at_offsets_monotonic_within_segment() {
    unimplemented!("Plan 10 — read_at_offsets_monotonic_within_segment scenario");
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn next_frame_count_matches_segment_duration() {
    unimplemented!("Plan 10 — next_frame_count_matches_segment_duration scenario");
}

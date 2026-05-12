//! Init range — `HlsSegmentView::init_segment_range` follows active variant.
//!
//! Spec: `.docs/plans/2026-05-11-abr-pull-driven-simplification.md#file-13`
//!
//! Plan 00 skeleton — bodies panic via `unimplemented!()`. Replaces T6.
//! Plan 10 fills the init-range scenarios.

use kithara_platform::time::Duration;
use kithara_test_utils::kithara;

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn init_range_symphonia() {
    unimplemented!("Plan 10 — init_range_symphonia scenario");
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn init_range_apple() {
    unimplemented!("Plan 10 — init_range_apple scenario");
}

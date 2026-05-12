//! Probes: `HlsSegmentView::segment_at_time`, `segment_after_byte`, `init_segment_range`.
//!
//! Spec: `.docs/plans/2026-05-11-abr-pull-driven-simplification.md#file-4`
//!
//! Plan 00 skeleton — bodies panic via `unimplemented!()`. Plan 04 fills
//! the lock-free SegmentLayout scenarios.

use kithara_platform::time::Duration;
use kithara_test_utils::kithara;

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn lockfree_concurrent_query_and_flip_no_torn_read() {
    unimplemented!("Plan 04 — lockfree_concurrent_query_and_flip_no_torn_read scenario");
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn init_segment_range_follows_active_variant() {
    unimplemented!("Plan 04 — init_segment_range_follows_active_variant scenario");
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn empty_variant_returns_none() {
    unimplemented!("Plan 04 — empty_variant_returns_none scenario");
}

//! Probes: `HlsVariant::find_at_offset`.
//!
//! Spec: `.docs/plans/2026-05-11-abr-pull-driven-simplification.md#file-8`
//!
//! Plan 00 skeleton — bodies panic via `unimplemented!()`. Plan 03 fills
//! the find-at-offset scenarios.

use kithara_platform::time::Duration;
use kithara_test_utils::kithara;

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn offset_below_init_size_resolves_to_init() {
    unimplemented!("Plan 03 — offset_below_init_size_resolves_to_init scenario");
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn offset_at_init_size_resolves_to_segment_0() {
    unimplemented!("Plan 03 — offset_at_init_size_resolves_to_segment_0 scenario");
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn offset_mid_segment_binary_search() {
    unimplemented!("Plan 03 — offset_mid_segment_binary_search scenario");
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn total_bytes_includes_init_plus_segments() {
    unimplemented!("Plan 03 — total_bytes_includes_init_plus_segments scenario");
}

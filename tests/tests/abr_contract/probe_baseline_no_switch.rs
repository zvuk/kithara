//! Baseline (no switch) — HTTP lifecycle, build_chunk progress, no ABR commits.
//!
//! Spec: `.docs/plans/2026-05-11-abr-pull-driven-simplification.md#file-10`
//!
//! Plan 00 skeleton — bodies panic via `unimplemented!()`. Replaces T8.
//! Plan 10 fills the baseline-no-switch scenarios.

use kithara_platform::time::Duration;
use kithara_test_utils::kithara;

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn baseline_aac_lq_symphonia() {
    unimplemented!("Plan 10 — baseline_aac_lq_symphonia scenario");
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn baseline_flac_symphonia() {
    unimplemented!("Plan 10 — baseline_flac_symphonia scenario");
}

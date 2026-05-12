//! Probes: `step_recreating_decoder`, `build_chunk` (unchanged audio FSM probes).
//!
//! Spec: `.docs/plans/2026-05-11-abr-pull-driven-simplification.md#file-9`
//!
//! Plan 00 skeleton — bodies panic via `unimplemented!()`. Replaces T1+T7.
//! Plan 10 fills the seam continuity parametric scenarios.

use kithara_platform::time::Duration;
use kithara_test_utils::kithara;

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn lq_to_hq_symphonia() {
    unimplemented!("Plan 10 — lq_to_hq_symphonia seam continuity scenario");
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn hq_to_flac_symphonia() {
    unimplemented!("Plan 10 — hq_to_flac_symphonia seam continuity scenario");
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn flac_to_lq_symphonia() {
    unimplemented!("Plan 10 — flac_to_lq_symphonia seam continuity scenario");
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn flac_to_lq_reverse_symphonia() {
    unimplemented!("Plan 10 — flac_to_lq_reverse_symphonia seam continuity scenario");
}

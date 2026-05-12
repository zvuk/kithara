//! Probes: `HlsVariant::dispatch` entry/exit.
//!
//! Spec: `.docs/plans/2026-05-11-abr-pull-driven-simplification.md#file-2`
//!
//! Plan 00 skeleton — bodies panic via `unimplemented!()`. Plan 03 fills
//! the probe-driven scenarios that exercise dispatch budget/init/skip/cancel.

use kithara_platform::time::Duration;
use kithara_test_utils::kithara;

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn dispatch_respects_budget() {
    unimplemented!("Plan 03 — dispatch_respects_budget scenario");
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn dispatch_emits_init_first() {
    unimplemented!("Plan 03 — dispatch_emits_init_first scenario");
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn dispatch_skips_non_missing_segments() {
    unimplemented!("Plan 03 — dispatch_skips_non_missing_segments scenario");
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn dispatch_attaches_variant_cancel_token() {
    unimplemented!("Plan 03 — dispatch_attaches_variant_cancel_token scenario");
}

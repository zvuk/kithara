//! Probes: `HlsVariant::on_evict`, `on_invalidated` callback.
//!
//! Spec: `.docs/plans/2026-05-11-abr-pull-driven-simplification.md#file-7`
//!
//! Plan 00 skeleton — bodies panic via `unimplemented!()`. Plan 08 fills
//! the eviction routing scenarios.

use kithara_platform::time::Duration;
use kithara_test_utils::kithara;

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn eviction_routed_only_to_owning_track() {
    unimplemented!("Plan 08 — eviction_routed_only_to_owning_track scenario");
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn on_evict_checks_init_then_segments() {
    unimplemented!("Plan 08 — on_evict_checks_init_then_segments scenario");
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn foreign_asset_id_ignored() {
    unimplemented!("Plan 08 — foreign_asset_id_ignored scenario");
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn active_variant_eviction_in_visibility_window_re_enqueues_front() {
    unimplemented!(
        "Plan 08 — active_variant_eviction_in_visibility_window_re_enqueues_front scenario"
    );
}

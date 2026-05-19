use kithara_platform::time::Duration;
use kithara_test_utils::kithara;

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
#[ignore = "Plan 10 — pending — probe wiring deferred per .docs/plans/2026-05-12-abr-pull-driven-10-H-test-sweep.md"]
async fn exactly_one_init_emit_per_variant() {
    unimplemented!("Plan 10 — exactly_one_init_emit_per_variant scenario");
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
#[ignore = "Plan 10 — pending — probe wiring deferred per .docs/plans/2026-05-12-abr-pull-driven-10-H-test-sweep.md"]
async fn zero_back_fetches_after_commit() {
    unimplemented!("Plan 10 — zero_back_fetches_after_commit scenario");
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
#[ignore = "Plan 10 — pending — probe wiring deferred per .docs/plans/2026-05-12-abr-pull-driven-10-H-test-sweep.md"]
async fn zero_overreach_beyond_buffer_target() {
    unimplemented!("Plan 10 — zero_overreach_beyond_buffer_target scenario");
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
#[ignore = "Plan 10 — pending — probe wiring deferred per .docs/plans/2026-05-12-abr-pull-driven-10-H-test-sweep.md"]
async fn no_duplicate_emits_per_segment() {
    unimplemented!("Plan 10 — no_duplicate_emits_per_segment scenario");
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
#[ignore = "Plan 10 — pending — probe wiring deferred per .docs/plans/2026-05-12-abr-pull-driven-10-H-test-sweep.md"]
async fn no_v_old_emits_after_variant_commit() {
    unimplemented!("Plan 10 — no_v_old_emits_after_variant_commit scenario");
}

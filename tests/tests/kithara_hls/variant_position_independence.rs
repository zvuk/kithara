use kithara_platform::time::Duration;
use kithara_test_utils::kithara;

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn v_old_and_v_new_positions_independent_after_flip() {
    unimplemented!("Plan 03 — v_old_and_v_new_positions_independent_after_flip scenario");
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn advance_only_changes_active_variant_position() {
    unimplemented!("Plan 03 — advance_only_changes_active_variant_position scenario");
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn position_monotonic_within_one_variant() {
    unimplemented!("Plan 03 — position_monotonic_within_one_variant scenario");
}

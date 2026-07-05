use kithara::{self, platform::time::Duration};

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
#[ignore = "pending — ABR probe wiring not implemented yet"]
async fn exactly_one_init_emit_per_variant() {
    unimplemented!("Plan 10 — exactly_one_init_emit_per_variant scenario");
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
#[ignore = "pending — ABR probe wiring not implemented yet"]
async fn zero_back_fetches_after_commit() {
    unimplemented!("Plan 10 — zero_back_fetches_after_commit scenario");
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
#[ignore = "pending — ABR probe wiring not implemented yet"]
async fn zero_overreach_beyond_buffer_target() {
    unimplemented!("Plan 10 — zero_overreach_beyond_buffer_target scenario");
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
#[ignore = "pending — ABR probe wiring not implemented yet"]
async fn no_duplicate_emits_per_segment() {
    unimplemented!("Plan 10 — no_duplicate_emits_per_segment scenario");
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
#[ignore = "pending — ABR probe wiring not implemented yet"]
async fn no_v_old_emits_after_variant_commit() {
    unimplemented!("Plan 10 — no_v_old_emits_after_variant_commit scenario");
}

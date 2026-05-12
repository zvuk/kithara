use kithara_platform::time::Duration;
use kithara_test_utils::kithara;

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn auto_commit_flips_active_variant() {
    unimplemented!("Plan 06 — auto_commit_flips_active_variant scenario");
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn manual_set_mode_uses_same_commit_path() {
    unimplemented!("Plan 06 — manual_set_mode_uses_same_commit_path scenario");
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn commit_pending_returns_none_during_seek() {
    unimplemented!("Plan 06 — commit_pending_returns_none_during_seek scenario");
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn did_change_false_skips_notify() {
    unimplemented!("Plan 06 — did_change_false_skips_notify scenario");
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn abr_frozen_between_seek_and_first_post_seek_chunk_t4() {
    unimplemented!("Plan 09 — abr_frozen_between_seek_and_first_post_seek_chunk_t4 scenario");
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn set_mode_to_first_post_chunk_latency_within_segment_duration_t5() {
    unimplemented!(
        "Plan 09 — set_mode_to_first_post_chunk_latency_within_segment_duration_t5 scenario"
    );
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn post_apply_emit_forward_only_t14() {
    unimplemented!("Plan 09 — post_apply_emit_forward_only_t14 scenario");
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn noop_set_mode_does_not_violate_forward_only_t14() {
    unimplemented!("Plan 09 — noop_set_mode_does_not_violate_forward_only_t14 scenario");
}

use kithara_platform::time::Duration;
use kithara_test_utils::kithara;

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn read_at_offsets_monotonic_within_segment() {
    unimplemented!("Plan 10 — read_at_offsets_monotonic_within_segment scenario");
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn next_frame_count_matches_segment_duration() {
    unimplemented!("Plan 10 — next_frame_count_matches_segment_duration scenario");
}

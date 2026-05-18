use kithara_platform::time::Duration;
use kithara_test_utils::kithara;

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
#[ignore = "Plan 10 — pending — probe wiring deferred per .docs/plans/2026-05-12-abr-pull-driven-10-H-test-sweep.md"]
async fn read_at_offsets_monotonic_within_segment() {
    unimplemented!("Plan 10 — read_at_offsets_monotonic_within_segment scenario");
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
#[ignore = "Plan 10 — pending — probe wiring deferred per .docs/plans/2026-05-12-abr-pull-driven-10-H-test-sweep.md"]
async fn next_frame_count_matches_segment_duration() {
    unimplemented!("Plan 10 — next_frame_count_matches_segment_duration scenario");
}

use kithara::{self, platform::time::Duration};

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
#[ignore = "Plan 10 — pending — probe wiring deferred per .docs/plans/2026-05-12-abr-pull-driven-10-H-test-sweep.md"]
async fn disk_files_match_dispatched_emits() {
    unimplemented!("Plan 10 — disk_files_match_dispatched_emits scenario");
}

use kithara::{self, platform::time::Duration};

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
#[ignore = "pending — ABR probe wiring not implemented yet"]
async fn disk_files_match_dispatched_emits() {
    unimplemented!("Plan 10 — disk_files_match_dispatched_emits scenario");
}

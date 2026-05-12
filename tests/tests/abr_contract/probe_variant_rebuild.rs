use kithara_platform::time::Duration;
use kithara_test_utils::kithara;

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn rebuild_on_seek_cancels_old_fetches() {
    unimplemented!("Plan 03 — rebuild_on_seek_cancels_old_fetches scenario");
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn variant_flip_cancels_v_old_token() {
    unimplemented!("Plan 03 — variant_flip_cancels_v_old_token scenario");
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn rebuild_front_of_queue_catch_up() {
    unimplemented!("Plan 03 — rebuild_front_of_queue_catch_up scenario");
}

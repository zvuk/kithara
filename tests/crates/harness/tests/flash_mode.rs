//! Ordinary platform time calls use the test mode in the body and its callees.
//! This test runs with and without the `flash` feature.

use kithara::{
    self,
    platform::time::{self, Duration, Instant, WallInstant},
};
use kithara_test_utils::pace;

async fn unannotated_callee_now() -> Instant {
    time::sleep(Duration::from_millis(1)).await;
    Instant::now()
}

#[kithara::test(native, tokio)]
async fn ordinary_now_uses_one_clock_across_calls() {
    let started = Instant::now();
    time::sleep(Duration::from_millis(10)).await;
    let callee_now = unannotated_callee_now().await;
    assert!(
        callee_now.saturating_duration_since(started) >= Duration::from_millis(10),
        "the body and callee must read the same clock"
    );
}

#[kithara::test(native)]
fn pace_uses_the_test_clock() {
    let started = Instant::now();
    pace(Duration::from_millis(10));
    assert!(started.elapsed() >= Duration::from_millis(10));
}

#[kithara::flash(false)]
async fn real_sleep() {
    time::sleep(Duration::from_millis(40)).await;
}

#[kithara::test(native, tokio)]
async fn flash_false_region_uses_real_time() {
    let started = WallInstant::now();
    real_sleep().await;
    assert!(started.elapsed() >= Duration::from_millis(25));
}

#[kithara::test(native, flash(false))]
fn flash_false_test_keeps_pace_real() {
    let started = WallInstant::now();
    pace(Duration::from_millis(10));
    assert!(started.elapsed() >= Duration::from_millis(10));
}

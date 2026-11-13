use std::hint;

use kithara::platform::{
    time::Duration,
    tokio::task::{spawn_blocking, yield_now},
};
use kithara_test_utils::test::wall_sleep;

#[kithara::test(native, flash(false))]
#[case::first("sync_timeout_preserves_the_harness_thread_name_first")]
#[case::second("sync_timeout_preserves_the_harness_thread_name_second")]
fn sync_timeout_preserves_the_harness_thread_name(#[case] expected: &str) {
    assert_eq!(std::thread::current().name(), Some(expected));
}

#[kithara::test(native, flash(false))]
#[kithara::hang_watchdog]
fn sync_timeout_preserves_watchdog_progress_and_result() -> Result<(), &'static str> {
    hang_reset!();
    hang_tick!();
    let value = "42".parse::<u32>().map_err(|_| "invalid integer")?;
    assert_eq!(value, 42);
    Ok(())
}

#[kithara::test(native)]
#[should_panic(expected = "timed out after 1s")]
fn default_flash_budget_bounds_wall_time() {
    wall_sleep(Duration::from_millis(1500));
}

#[kithara::test(native, flash(false), timeout(Medium))]
fn named_real_budget_allows_four_wall_seconds() {
    wall_sleep(Duration::from_secs(4));
}

#[kithara::test(native, timeout(Duration::from_secs(2)))]
#[serial_test::serial]
#[should_panic(expected = "timed out")]
fn sync_infinite_loop_is_killed_by_timeout() {
    loop {
        hint::spin_loop();
    }
}

#[kithara::test(tokio, timeout(Duration::from_secs(2)))]
#[serial_test::serial]
#[cfg_attr(not(target_arch = "wasm32"), should_panic(expected = "timed out"))]
#[cfg_attr(target_arch = "wasm32", should_panic)]
async fn async_infinite_loop_is_killed_by_timeout() {
    loop {
        yield_now().await;
    }
}

#[kithara::test(tokio, timeout(Duration::from_secs(2)))]
#[serial_test::serial]
#[cfg_attr(not(target_arch = "wasm32"), should_panic(expected = "timed out"))]
#[cfg_attr(target_arch = "wasm32", should_panic)]
async fn async_spawn_blocking_zombie_is_killed_by_timeout() {
    let _handle = spawn_blocking(|| {
        loop {
            hint::spin_loop();
        }
    })
    .await;
}

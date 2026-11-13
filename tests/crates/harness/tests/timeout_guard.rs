use std::hint;

use kithara::platform::{
    time::Duration,
    tokio::task::{spawn_blocking, yield_now},
};
use kithara_test_utils::test::wall_sleep;

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

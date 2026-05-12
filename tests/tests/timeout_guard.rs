use std::hint;

use kithara_platform::{
    time::Duration,
    tokio::task::{spawn_blocking, yield_now},
};

#[kithara::test(native, serial, timeout(Duration::from_secs(2)))]
#[should_panic(expected = "timed out")]
fn sync_infinite_loop_is_killed_by_timeout() {
    loop {
        hint::spin_loop();
    }
}

#[kithara::test(tokio, serial, timeout(Duration::from_secs(2)))]
#[cfg_attr(not(target_arch = "wasm32"), should_panic(expected = "timed out"))]
#[cfg_attr(target_arch = "wasm32", should_panic)]
async fn async_infinite_loop_is_killed_by_timeout() {
    loop {
        yield_now().await;
    }
}

#[kithara::test(tokio, serial, timeout(Duration::from_secs(2)))]
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

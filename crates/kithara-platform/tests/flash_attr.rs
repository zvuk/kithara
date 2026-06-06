//! `#[kithara::flash(bool)]` PROD dynamic-guard (B3): a prod fn annotated with
//! the attribute propagates flash dynamically through its body. Under ambient
//! (a flash-eligible test) `flash(true)` collapses virtual waits; without ambient
//! it is a no-op so waits stay REAL. Gated on `flash-time` (native only).
#![cfg(all(not(target_arch = "wasm32"), feature = "flash-time"))]

use std::time::Instant as RealInstant;

use kithara_platform::time::{self, Duration, ambient_scope};

#[kithara_test_macros::flash(true)]
async fn does_sleep() {
    time::sleep(Duration::from_secs(3600)).await;
}

#[test]
fn prod_flash_collapses_under_ambient() {
    let _a = ambient_scope(true);
    let t0 = RealInstant::now();
    // 1h VIRTUAL collapses to ~0 real: the engine advances the clock once the
    // lone `FlashSleep` is the only parked waiter (no spawn participant pins
    // `active_async`), so a bare `block_on` drives the collapse.
    futures::executor::block_on(does_sleep());
    assert!(
        t0.elapsed() < Duration::from_secs(1),
        "flash(true) under ambient collapsed the 1h sleep"
    );
}

#[test]
fn prod_flash_real_without_ambient() {
    // No ambient: `enter_dynamic(true)` gates on ambient, so `flash(true)` is a
    // no-op and the sleep is REAL. A real `time::sleep` is a `tokio` timer, so it
    // is driven on a tokio runtime; a SHORT real sleep keeps the test fast while
    // proving the wait stayed real.
    #[kithara_test_macros::flash(true)]
    async fn short_real_sleep() {
        time::sleep(Duration::from_millis(40)).await;
    }
    let rt = kithara_platform::tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("current-thread runtime must build");
    let t0 = RealInstant::now();
    rt.block_on(short_real_sleep());
    assert!(
        t0.elapsed() >= Duration::from_millis(25),
        "without ambient the sleep stayed real"
    );
}

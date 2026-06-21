//! `#[kithara::flash(bool)]` PROD dynamic-guard (B3): a prod fn annotated with
//! the attribute propagates flash dynamically through its body. Under ambient
//! (a flash-eligible test) `flash(true)` collapses virtual waits; without ambient
//! it is a no-op so waits stay REAL. `#[kithara::flash(io)]` brackets the body
//! as ONE real I/O op: the virtual clock is paced to real time while it is in
//! flight. Gated on `flash` (native only).
#![cfg(all(not(target_arch = "wasm32"), feature = "flash"))]

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Instant as RealInstant,
};

use kithara_platform::{
    flash::ambient_scope,
    time::{self, Duration},
};

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

/// One real-I/O op: the guard is injected before the body, lives in the
/// future's state across the `.await`, and drops when the future completes.
#[kithara_test_macros::flash(io)]
async fn io_op(entered: Arc<AtomicBool>, rx: futures::channel::oneshot::Receiver<()>) {
    entered.store(true, Ordering::SeqCst);
    let _ = rx.await;
}

#[kithara_test_macros::flash(true)]
async fn short_virtual_sleep() {
    time::sleep(Duration::from_millis(100)).await;
}

#[test]
fn prod_flash_io_paces_virtual_clock_across_await() {
    let _a = ambient_scope(true);
    let (tx, rx) = futures::channel::oneshot::channel::<()>();
    let entered = Arc::new(AtomicBool::new(false));
    let entered_io = Arc::clone(&entered);
    let io_thread = thread::spawn(move || futures::executor::block_on(io_op(entered_io, rx)));
    while !entered.load(Ordering::SeqCst) {
        thread::yield_now();
    }

    // The annotated op is parked mid-await: its `flash(io)` guard paces the
    // clock, so a 100ms VIRTUAL sleep must take ~100ms REAL instead of
    let t0 = RealInstant::now();
    futures::executor::block_on(short_virtual_sleep());
    let paced = t0.elapsed();
    assert!(
        paced >= Duration::from_millis(60),
        "virtual sleep must be paced to real time while a flash(io) op is in flight: {paced:?}"
    );

    // Releasing the op (guard drops with the future) resumes full collapse —
    // a leaked counter would leave the 1h sleep below paced (the test hangs).
    tx.send(()).ok();
    io_thread.join().expect("io thread panicked");
    let t1 = RealInstant::now();
    futures::executor::block_on(does_sleep());
    assert!(
        t1.elapsed() < Duration::from_secs(1),
        "full-speed collapse must resume once the flash(io) op completes"
    );
}

#[test]
fn prod_flash_real_without_ambient() {
    // No ambient: `enter_dynamic(true)` gates on ambient, so `flash(true)` is a
    // no-op and the sleep is REAL. A real `time::sleep` is a `tokio` timer, so it
    // is driven on a tokio runtime; a SHORT real sleep keeps the test fast while
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

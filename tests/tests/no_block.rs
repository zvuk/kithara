use std::sync::Arc;

use kithara::platform::{
    sync::{Condvar, Mutex},
    thread,
    time::Duration,
};
use kithara_integration_tests::kithara;

#[kithara::test(flash(false), env(KITHARA_NO_BLOCK = "panic"))]
#[should_panic(expected = "[no_block]")]
async fn blanket_catches_platform_sleep_in_test_body() {
    thread::sleep(Duration::from_millis(1));
}

#[kithara::test(env(KITHARA_NO_BLOCK = "panic"))]
#[should_panic(expected = "[no_block]")]
async fn blanket_catches_bridged_wait_under_flash() {
    let pair: Arc<(Mutex<bool>, Condvar)> = Arc::new((Mutex::new(false), Condvar::default()));
    let signaler = Arc::clone(&pair);
    thread::spawn_named("nb-signaler", move || {
        let (lock, cvar) = signaler.as_ref();
        thread::sleep(Duration::from_millis(5));
        *lock.lock() = true;
        cvar.notify_all();
    });

    let (lock, cvar) = pair.as_ref();
    let mut guard = lock.lock();
    while !*guard {
        guard = cvar.wait(guard);
    }
}

#[kithara::test(env(KITHARA_NO_BLOCK = "panic"))]
async fn allow_block_bridge_passes() {
    let pair: Arc<(Mutex<bool>, Condvar)> = Arc::new((Mutex::new(false), Condvar::default()));
    let signaler = Arc::clone(&pair);
    thread::spawn_named("nb-signaler-ok", move || {
        let (lock, cvar) = signaler.as_ref();
        thread::sleep(Duration::from_millis(5));
        *lock.lock() = true;
        cvar.notify_all();
    });
    sanctioned_bridge(&pair);
}

#[kithara::allow_block]
fn sanctioned_bridge(pair: &Arc<(Mutex<bool>, Condvar)>) {
    let (lock, cvar) = pair.as_ref();
    let mut guard = lock.lock();
    while !*guard {
        guard = cvar.wait(guard);
    }
}

use kithara::platform::{
    no_block::force_panic_mode,
    sync::{Arc, Condvar, Mutex},
    thread,
    time::Duration,
};
use kithara_integration_tests::kithara;

#[kithara::test(flash(false))]
#[should_panic(expected = "[no_block]")]
async fn blanket_catches_platform_sleep_in_test_body() {
    // These three judge the watcher itself, so they force `panic` mode: the
    // stress lanes run the suite in `census`, where a blocking wait only logs.
    let _mode = force_panic_mode();
    thread::sleep(Duration::from_millis(1));
}

#[kithara::test]
#[should_panic(expected = "[no_block]")]
async fn blanket_catches_bridged_wait_under_flash() {
    let _mode = force_panic_mode();
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

#[kithara::test]
async fn allow_block_bridge_passes() {
    let _mode = force_panic_mode();
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

#[cfg(feature = "flash")]
use kithara::platform::{
    flash::ambient_snapshot,
    sync::{ThreadGate, WaitGate},
    time::Duration,
};
use kithara::platform::{
    sync::{
        Arc, Condvar, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
};

/// A backstop, not a scheduling budget. The gate tests assert that a signal
/// published after the waiter's snapshot wakes that waiter; a correct gate
/// returns as soon as the signal lands, so this deadline never becomes runtime.
/// Under the flash engine the deadline is virtual: the clock jumps straight to
/// it the moment no counted participant is left running. The test body itself
/// is NOT counted, so every peer a waiter depends on must be spawned (its slot
/// reserved) BEFORE the waiter can park — otherwise the jump fires in the gap.
#[cfg(feature = "flash")]
const SIGNAL_BACKSTOP: Duration = Duration::from_secs(5);

#[kithara::test(loom)]
fn platform_mutex_is_explored_by_loom() {
    let value = Arc::new(Mutex::new(0));
    let other = Arc::clone(&value);

    let handle = thread::spawn(move || {
        *other.lock() += 1;
    });
    *value.lock() += 1;

    assert!(handle.join().is_ok());
    assert_eq!(*value.lock(), 2);
}

/// `platform_condvar_is_explored_by_loom`'s loop body: the guard enters as a
/// parameter and drops on return — same hold window as an in-line loop, but
/// keeps clippy's `significant_drop_tightening` from mis-suggesting an early
/// drop inside the loop. Mirrors
/// `kithara::platform::flash::system::wake::wait_set`.
fn wait_until_ready(condvar: &Condvar, mut ready: MutexGuard<'_, bool>) {
    while !*ready {
        ready = condvar.wait(ready);
    }
}

#[kithara::test(loom)]
fn platform_condvar_is_explored_by_loom() {
    let state = Arc::new((Mutex::new(false), Condvar::default()));
    let waiter_state = Arc::clone(&state);
    let waiter = thread::spawn(move || {
        let (lock, condvar) = &*waiter_state;
        wait_until_ready(condvar, lock.lock());
    });

    let (lock, condvar) = &*state;
    *lock.lock() = true;
    condvar.notify_one();

    assert!(waiter.join().is_ok());
}

#[kithara::test(loom)]
fn platform_channel_is_explored_by_loom() {
    let (sender, receiver) = mpsc::channel();
    let producer = thread::spawn(move || sender.send(7));

    assert_eq!(receiver.recv(), Ok(7));
    assert!(matches!(producer.join(), Ok(Ok(()))));
}

#[kithara::test(loom)]
fn platform_atomics_are_explored_by_loom() {
    let published = Arc::new(AtomicBool::new(false));
    let producer_flag = Arc::clone(&published);
    let producer = thread::spawn(move || {
        producer_flag.store(true, Ordering::Release);
    });

    while !published.load(Ordering::Acquire) {
        thread::yield_now();
    }

    assert!(producer.join().is_ok());
}

#[cfg(feature = "flash")]
#[kithara::test(loom)]
fn platform_spawn_propagates_flash_ambient() {
    let child = thread::spawn(ambient_snapshot);

    assert!(matches!(child.join(), Ok(true)));
}

#[cfg(feature = "flash")]
#[kithara::test(loom)]
fn thread_gate_closes_hls_snapshot_probe_park_race() {
    let gate = Arc::new(ThreadGate::default());
    let ready = Arc::new(AtomicBool::new(false));
    let since = gate.current();

    let producer_gate = Arc::clone(&gate);
    let producer_ready = Arc::clone(&ready);
    let producer = thread::spawn(move || {
        producer_ready.store(true, Ordering::Release);
        producer_gate.signal();
    });

    if !ready.load(Ordering::Acquire) {
        assert!(gate.wait_timeout(since, SIGNAL_BACKSTOP));
    }
    assert!(ready.load(Ordering::Acquire));
    assert!(producer.join().is_ok());
}

#[cfg(feature = "flash")]
#[kithara::test(loom)]
fn thread_gate_refreshes_waiter_after_thread_handoff() {
    let gate = Arc::new(ThreadGate::default());

    let first_since = gate.current();
    gate.signal();
    let first_gate = Arc::clone(&gate);
    let first = thread::spawn(move || first_gate.wait_timeout(first_since, SIGNAL_BACKSTOP));
    assert!(matches!(first.join(), Ok(true)));

    let (snapshot_tx, snapshot_rx) = mpsc::channel();
    // Spawn the signaller FIRST: `spawn_named` reserves its engine slot on this
    // (engine-invisible) thread, so the waiter is never the last credit holder while
    // parked on the backstop — the send's `notify_one` bumps `active` for the woken
    // signaller under the core lock before the waiter's own park drops its credit.
    // Waiter first, and it parks with nothing counted: the clock jumps the whole
    // backstop before the signaller exists.
    let second_gate = Arc::clone(&gate);
    let signaller = thread::spawn_named("threadgate-handoff-signaller", move || {
        assert_eq!(snapshot_rx.recv(), Ok(()));
        gate.signal();
    });
    let second = thread::spawn(move || {
        let since = second_gate.current();
        if snapshot_tx.send(()).is_err() {
            return false;
        }
        second_gate.wait_timeout(since, SIGNAL_BACKSTOP)
    });
    assert!(matches!(second.join(), Ok(true)));
    assert!(signaller.join().is_ok());
}

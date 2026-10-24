//! Public-surface tests for the blocking `sync::mpsc` channel and the
//! `thread` primitives. Moved from the W1 facade files (`src/sync/mpsc.rs`,
//! `src/thread.rs`) when the facades died in 1.11b — they exercise the public
//! surface, so they live against it.

#[cfg(all(test, target_os = "android"))]
use kithara_test_dylib as _;

#[cfg(not(target_arch = "wasm32"))]
mod mutex_bounds {
    use std::rc::Rc;

    use kithara_platform::sync::Mutex;

    const _: bool = {
        trait AmbiguousIfSend<A> {
            const CHECK: bool;
        }
        impl<T: ?Sized> AmbiguousIfSend<()> for T {
            const CHECK: bool = true;
        }
        impl<T: ?Sized + Send> AmbiguousIfSend<u8> for T {
            const CHECK: bool = true;
        }
        <Mutex<Rc<()>> as AmbiguousIfSend<_>>::CHECK
    };

    const _: bool = {
        trait AmbiguousIfSync<A> {
            const CHECK: bool;
        }
        impl<T: ?Sized> AmbiguousIfSync<()> for T {
            const CHECK: bool = true;
        }
        impl<T: ?Sized + Sync> AmbiguousIfSync<u8> for T {
            const CHECK: bool = true;
        }
        <Mutex<Rc<()>> as AmbiguousIfSync<_>>::CHECK
    };
}

#[cfg(not(target_arch = "wasm32"))]
mod clock {
    use kithara_platform::{
        thread,
        time::{Duration, Instant, WallInstant},
    };
    use kithara_test_utils::kithara;

    #[kithara::test]
    fn ordinary_now_and_sleep_use_the_same_clock() {
        let started = Instant::now();
        thread::sleep(Duration::from_millis(5));
        assert!(started.elapsed() >= Duration::from_millis(5));
    }

    #[kithara::test(flash(false))]
    fn flash_false_keeps_real_time() {
        let started = WallInstant::now();
        thread::sleep(Duration::from_millis(5));
        assert!(started.elapsed() >= Duration::from_millis(5));
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod mpsc {
    use std::time::Duration;

    use kithara_platform::{sync::mpsc::*, time::Instant};
    use kithara_test_utils::kithara;

    /// The same public `Instant::now()` deadline must work in both clock lanes.
    #[kithara::test]
    fn recv_sync_timeout_returns_delivered_value_before_deadline() {
        let (tx, rx) = channel::<u32>();
        tx.send(7).expect("send to live receiver");
        let deadline = Instant::now() + Duration::from_secs(1);
        assert_eq!(rx.recv_timeout(deadline), Ok(7));
    }

    #[kithara::test]
    fn recv_sync_timeout_times_out_when_no_value_arrives() {
        let (_tx, rx) = channel::<u32>();
        let deadline = Instant::now() + Duration::from_millis(10);
        assert_eq!(rx.recv_timeout(deadline), Err(RecvTimeoutError::Timeout));
    }

    #[kithara::test]
    fn recv_sync_timeout_reports_disconnect_when_senders_dropped() {
        let (tx, rx) = channel::<u32>();
        drop(tx);
        let deadline = Instant::now() + Duration::from_secs(1);
        assert_eq!(
            rx.recv_timeout(deadline),
            Err(RecvTimeoutError::Disconnected)
        );
    }
}

mod thread {
    use std::time::Instant;

    use kithara_platform::thread::*;
    use kithara_test_utils::kithara;

    #[kithara::test]
    fn native_thread_detectors_are_consistent() {
        #[cfg(not(target_arch = "wasm32"))]
        {
            assert!(is_main_thread());
            assert!(!is_worker_thread());
            assert_main_thread("native-main");
            assert_not_main_thread("native-main");
        }
    }

    /// This timing assertion measures real wall time, including under flash.
    #[kithara::test(flash(false))]
    fn park_timeout_returns_after_unpark() {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let parked = current();
            let start = Instant::now();
            let join = spawn(move || {
                sleep(Duration::from_millis(5));
                parked.unpark();
            });
            park_timeout(Duration::from_secs(1));
            join.join()
                .expect("BUG: wake-helper thread joined cleanly without panicking");
            assert!(start.elapsed() < Duration::from_millis(250));
        }
    }
}

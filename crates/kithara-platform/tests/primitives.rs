//! Public-surface tests for the blocking `sync::mpsc` channel and the
//! `thread` primitives. Moved from the W1 facade files (`src/sync/mpsc.rs`,
//! `src/thread.rs`) when the facades died in 1.11b — they exercise the public
//! surface, so they live against it.

#[cfg(not(target_arch = "wasm32"))]
mod mpsc {
    use std::time::Duration;

    use kithara_platform::{sync::mpsc::*, time::Instant};
    use kithara_test_utils::kithara;

    // `flash(false)`: these tests build a `kithara_platform::time::Instant`
    // deadline and pass it to the crate's own `recv_sync_timeout`. The lexical
    // flash rewrite would retarget `Instant::now()` onto
    // `::kithara_platform::flash::virtual_now`, whose `Instant` resolves through
    // the dev-dep copy of `kithara_platform` (`kithara-test-utils` pulls a
    // non-flash copy) — a different `Instant` type than the one the method
    // expects. They do not test flash time behaviour, so opting out of the
    // rewrite keeps a single `Instant`.
    #[kithara::test(flash(false))]
    fn recv_sync_timeout_returns_delivered_value_before_deadline() {
        let (tx, rx) = channel::<u32>();
        tx.send_sync(7).expect("send to live receiver");
        let deadline = Instant::now() + Duration::from_secs(1);
        assert_eq!(rx.recv_sync_timeout(deadline), Ok(7));
    }

    #[kithara::test(flash(false))]
    fn recv_sync_timeout_times_out_when_no_value_arrives() {
        let (_tx, rx) = channel::<u32>();
        let deadline = Instant::now() + Duration::from_millis(10);
        assert_eq!(
            rx.recv_sync_timeout(deadline),
            Err(RecvTimeoutError::Timeout)
        );
    }

    #[kithara::test(flash(false))]
    fn recv_sync_timeout_reports_disconnect_when_senders_dropped() {
        let (tx, rx) = channel::<u32>();
        drop(tx);
        let deadline = Instant::now() + Duration::from_secs(1);
        assert_eq!(
            rx.recv_sync_timeout(deadline),
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

    // `flash(false)`: a real park/unpark timing test. It measures REAL
    // wall-clock with `std::time::Instant` (not the platform clock) and asserts
    // the unpark wakes within 250ms. The lexical flash rewrite would retarget
    // `Instant::now()` onto the engine `virtual_now`, changing the clock
    // and leaving the `std::time::Instant` import unused, so opt out.
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

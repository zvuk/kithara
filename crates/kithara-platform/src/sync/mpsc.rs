#[cfg(all(not(target_arch = "wasm32"), feature = "flash"))]
pub use crate::flash::sync::mpsc::*;
#[cfg(all(not(target_arch = "wasm32"), not(feature = "flash")))]
pub use crate::native::sync::mpsc::*;
#[cfg(target_arch = "wasm32")]
pub use crate::wasm::sync::mpsc::*;

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use std::time::Duration;

    use kithara_test_utils::kithara;

    use super::*;
    use crate::time::Instant;

    // `flash(false)`: these unit tests build a `crate::time::Instant` deadline and
    // pass it to the crate's own `recv_sync_timeout`. The lexical flash rewrite
    // would retarget `Instant::now()` onto `::kithara_platform::flash::virtual_now`,
    // whose `Instant` resolves through the dev-dep copy of `kithara_platform`
    // (`kithara-test-utils` pulls a non-flash copy) — a different `Instant` type
    // than the crate-local one the method expects. They do not test flash time
    // behaviour, so opting out of the rewrite keeps a single, crate-local `Instant`.
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

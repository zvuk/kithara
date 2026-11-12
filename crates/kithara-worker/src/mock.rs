pub use crate::wake::wake_state;

/// Consume the same pending wake that starts the scheduler's next pass.
#[must_use]
pub fn flush_deferred(wake: &crate::Wake) -> bool {
    wake.wait_timeout(kithara_platform::time::Duration::ZERO)
}

/// Wait up to `timeout` for the wake that starts the scheduler's next pass.
#[must_use]
pub fn wait_wake(wake: &crate::Wake, timeout: kithara_platform::time::Duration) -> bool {
    wake.wait_poll_timeout(timeout)
}

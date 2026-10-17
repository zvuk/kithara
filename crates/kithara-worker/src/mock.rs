pub use crate::wake::wake_state;

/// Consume the same pending wake that starts the scheduler's next pass.
#[must_use]
pub fn flush_deferred(wake: &crate::Wake) -> bool {
    wake.wait_timeout(kithara_platform::time::Duration::ZERO)
}

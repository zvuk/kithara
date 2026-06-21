/// `try_lock()` failed because the mutex is already held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotAvailable;

impl std::fmt::Display for NotAvailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("mutex is already locked")
    }
}

impl std::error::Error for NotAvailable {}

/// `try_lock()` failed because the mutex is already held.
#[derive(Debug, Clone, Copy, derive_more::Display, PartialEq, Eq)]
#[display("mutex is already locked")]
pub struct NotAvailable;

impl std::error::Error for NotAvailable {}

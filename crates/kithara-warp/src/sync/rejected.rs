use std::fmt;

use super::{SyncError, SyncGroup, SyncOperation};

/// A rejected transaction together with the operation whose ownership was not accepted.
#[derive(derive_more::Display, derive_more::Error, fieldwork::Fieldwork)]
#[display("{error}")]
#[error(ignore)]
#[fieldwork(opt_in, get)]
#[non_exhaustive]
pub struct SyncRejected<G: SyncGroup> {
    /// Returns the reason the transaction was rejected.
    #[field(get)]
    #[error(source)]
    error: SyncError,
    /// Returns the still-owned operation that was not committed.
    #[field(get)]
    operation: Box<SyncOperation<G>>,
}

impl<G: SyncGroup> SyncRejected<G> {
    /// Preserves a failed operation for inspection or explicit disposal.
    #[must_use]
    pub fn new(error: SyncError, operation: SyncOperation<G>) -> Self {
        Self {
            error,
            operation: Box::new(operation),
        }
    }
}

impl<G: SyncGroup> From<SyncRejected<G>> for (SyncError, SyncOperation<G>) {
    fn from(rejected: SyncRejected<G>) -> Self {
        (rejected.error, *rejected.operation)
    }
}

impl<G: SyncGroup> fmt::Debug for SyncRejected<G> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SyncRejected")
            .field("error", &self.error)
            .field("operation_target", &self.operation.target())
            .finish_non_exhaustive()
    }
}

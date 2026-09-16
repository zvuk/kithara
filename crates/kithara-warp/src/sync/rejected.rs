use std::fmt;

use super::{SyncError, SyncGroup, SyncOperation};

/// A rejected transaction together with the operation whose ownership was not accepted.
#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
#[non_exhaustive]
pub struct SyncRejected<G: SyncGroup> {
    /// Returns the reason the transaction was rejected.
    #[field(get)]
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

impl<G: SyncGroup> fmt::Display for SyncRejected<G> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(formatter)
    }
}

impl<G: SyncGroup> std::error::Error for SyncRejected<G> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

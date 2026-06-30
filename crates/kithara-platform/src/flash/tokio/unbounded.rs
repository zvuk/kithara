use std::{fmt, sync::Arc};

use super::mpsc::{Shared, drop_sender, error::SendError, push_unbounded};

/// Unbounded sender (clone for multi-producer); `send` never blocks.
pub struct UnboundedSender<T> {
    pub(super) shared: Arc<Shared<T>>,
}

impl<T> fmt::Debug for UnboundedSender<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UnboundedSender").finish_non_exhaustive()
    }
}

impl<T> UnboundedSender<T> {
    /// Enqueue `value` without blocking.
    ///
    /// # Errors
    /// Returns [`SendError`] (carrying `value`) when the receiver has dropped.
    pub fn send(&self, value: T) -> Result<(), SendError<T>> {
        push_unbounded(&self.shared, value)
    }
}

impl<T> Clone for UnboundedSender<T> {
    fn clone(&self) -> Self {
        self.shared.add_sender();
        Self {
            shared: Arc::clone(&self.shared),
        }
    }
}

impl<T> Drop for UnboundedSender<T> {
    fn drop(&mut self) {
        drop_sender(&self.shared);
    }
}

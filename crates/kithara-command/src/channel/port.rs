use crate::{Batch, Protocol, SendError, Sender, Seq, When};

/// One level's command destination and projected basis.
pub trait Port<P: Protocol> {
    /// Sends a whole batch without spending a number on failure.
    ///
    /// # Errors
    /// Returns the batch as [`SendError::Target`], [`SendError::Closed`] or [`SendError::Full`].
    fn send(&mut self, when: When<P::Clock>, batch: Batch<P>) -> Result<Seq, SendError<P>>;

    /// The last projected shift of `target` at `when`.
    fn basis(&self, target: P::Target, when: When<P::Clock>) -> Option<Seq>;

    /// Batches this level can still send before returning [`SendError::Full`].
    fn available(&self) -> usize;
}

impl<P: Protocol> Port<P> for Sender<P> {
    delegate::delegate! {
        to self {
            fn send(&mut self, when: When<P::Clock>, batch: Batch<P>) -> Result<Seq, SendError<P>>;
            fn basis(&self, target: P::Target, when: When<P::Clock>) -> Option<Seq>;
            fn available(&self) -> usize;
        }
    }
}

/// All senders dropped; the watched value will never change again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecvError;

impl std::fmt::Display for RecvError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("watch channel closed")
    }
}

impl std::error::Error for RecvError {}

/// Returned by `Sender::send` when no receivers remain; carries the value back.
/// Distinct from `tokio`'s (its inner field is private); callers discard it.
pub struct SendError<T>(pub T);

impl<T> std::fmt::Debug for SendError<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SendError(..)")
    }
}

impl<T> std::fmt::Display for SendError<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("sending on a watch channel with no receivers")
    }
}

impl<T> std::error::Error for SendError<T> {}

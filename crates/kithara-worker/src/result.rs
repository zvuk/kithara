//! Worker execution result types.

/// Result of a single worker iteration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerResult {
    /// Continue processing.
    Continue,
    /// End of data reached.
    Eof,
    /// Stop worker (error or channel closed).
    Stop,
}

//! Worker execution result types.

use bitflags::bitflags;

bitflags! {
    /// Worker result flags.
    ///
    /// Can be combined to express multiple states simultaneously.
    /// Note: STOP and CONTINUE are mutually exclusive and should not be set together.
    #[repr(transparent)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct WorkerResult: u8 {
        /// Continue processing (no special state).
        const CONTINUE = 0b0000_0001;
        /// Command was received and processed.
        const COMMAND_RECEIVED = 0b0000_0010;
        /// End of data reached.
        const EOF = 0b0000_0100;
        /// Stop worker (error or channel closed).
        const STOP = 0b0000_1000;
    }
}

/// Trait for convenient bool access to worker result flags.
///
/// Allows code without `bitflags` dependency to work with results.
pub trait WorkerResultExt {
    /// Check if worker should continue processing.
    fn is_continue(&self) -> bool;

    /// Check if a command was received.
    fn is_command_received(&self) -> bool;

    /// Check if EOF was reached.
    fn is_eof(&self) -> bool;

    /// Check if worker should stop.
    fn is_stop(&self) -> bool;
}

impl WorkerResultExt for WorkerResult {
    fn is_continue(&self) -> bool {
        self.contains(WorkerResult::CONTINUE)
    }

    fn is_command_received(&self) -> bool {
        self.contains(WorkerResult::COMMAND_RECEIVED)
    }

    fn is_eof(&self) -> bool {
        self.contains(WorkerResult::EOF)
    }

    fn is_stop(&self) -> bool {
        self.contains(WorkerResult::STOP)
    }
}

impl WorkerResult {
    /// Create a simple "continue" result.
    pub fn continue_processing() -> Self {
        WorkerResult::CONTINUE
    }

    /// Create a "command received" result.
    pub fn command_received() -> Self {
        WorkerResult::COMMAND_RECEIVED
    }

    /// Create an "EOF" result.
    pub fn eof() -> Self {
        WorkerResult::EOF
    }

    /// Create a "stop" result.
    pub fn stop() -> Self {
        WorkerResult::STOP
    }

    /// Validate that mutually exclusive flags are not set together.
    ///
    /// Returns `Err` if both STOP and CONTINUE are set.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.contains(WorkerResult::STOP) && self.contains(WorkerResult::CONTINUE) {
            return Err("STOP and CONTINUE are mutually exclusive");
        }
        Ok(())
    }

    /// Check if the result indicates the worker should terminate.
    pub fn should_terminate(&self) -> bool {
        self.contains(WorkerResult::STOP)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bitflags_basic() {
        let continue_result = WorkerResult::CONTINUE;
        assert!(continue_result.is_continue());
        assert!(!continue_result.is_stop());
    }

    #[test]
    fn test_combine_flags() {
        let combined = WorkerResult::EOF | WorkerResult::CONTINUE;
        assert!(combined.is_eof());
        assert!(combined.is_continue());
        assert!(!combined.is_stop());
    }

    #[test]
    fn test_validation_mutually_exclusive() {
        let invalid = WorkerResult::STOP | WorkerResult::CONTINUE;
        assert!(invalid.validate().is_err());

        let valid = WorkerResult::EOF | WorkerResult::CONTINUE;
        assert!(valid.validate().is_ok());
    }
}

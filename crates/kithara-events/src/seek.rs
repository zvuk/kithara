#![forbid(unsafe_code)]

/// Monotonic generation identifier for seek operations.
pub type SeekEpoch = u64;

/// Stable task identifier for correlating seek lifecycle diagnostics.
pub type SeekTaskId = u64;

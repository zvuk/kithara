pub use core::time::Duration;

pub(crate) use web_time::Instant;
pub use web_time::SystemTime;

/// Error returned when an async operation exceeds its deadline.
#[derive(Debug, derive_more::Display)]
#[display("operation timed out")]
pub struct TimeoutError;

impl std::error::Error for TimeoutError {}

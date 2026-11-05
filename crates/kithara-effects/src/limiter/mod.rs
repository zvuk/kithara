mod effect;
mod parameters;
#[cfg(test)]
mod scalar;

pub use effect::{LimiterConfig, LimiterError, PeakLimiter};
pub use parameters::{Ceiling, CeilingError, ReleaseMs, ReleaseMsError};

#[cfg(feature = "mock")]
mod real;

#[cfg(feature = "mock")]
pub use real::*;

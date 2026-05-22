#[cfg(feature = "hang")]
pub(crate) mod real;
#[cfg(feature = "hang")]
pub use real::*;

#[cfg(not(feature = "hang"))]
mod noop;
#[cfg(not(feature = "hang"))]
pub use noop::*;

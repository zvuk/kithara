#[cfg(not(feature = "usdt"))]
mod noop;
#[cfg(feature = "usdt")]
mod real;

#[cfg(not(feature = "usdt"))]
pub use noop::*;
#[cfg(feature = "usdt")]
pub use real::*;

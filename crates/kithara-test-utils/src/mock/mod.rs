#![cfg_attr(not(feature = "mock"), allow(unreachable_pub))]

#[cfg(not(feature = "mock"))]
mod noop;
#[cfg(feature = "mock")]
mod real;

#[cfg(not(feature = "mock"))]
pub use noop::*;
#[cfg(feature = "mock")]
pub use real::*;

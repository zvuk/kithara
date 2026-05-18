#![cfg_attr(not(feature = "test"), allow(unreachable_pub))]

#[cfg(not(feature = "test"))]
mod noop;
#[cfg(feature = "test")]
mod real;

#[cfg(not(feature = "test"))]
pub use noop::*;
#[cfg(feature = "test")]
pub use real::*;

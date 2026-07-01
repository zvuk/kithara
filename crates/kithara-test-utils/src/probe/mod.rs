#[cfg(all(not(target_arch = "wasm32"), any(test, feature = "probe")))]
pub mod capture;

#[cfg(not(feature = "probe"))]
mod noop;
#[cfg(feature = "probe")]
mod real;

#[cfg(not(feature = "probe"))]
pub use noop::*;
#[cfg(feature = "probe")]
pub use real::*;

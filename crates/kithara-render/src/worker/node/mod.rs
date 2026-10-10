mod core;
#[cfg(all(test, feature = "mock"))]
pub(crate) mod mock;
mod pending;

pub use core::DecoderNode;

#[cfg(test)]
mod tests;

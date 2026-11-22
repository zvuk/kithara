mod core;
mod design;
#[cfg(feature = "routes")]
pub(crate) mod measured;

#[cfg(test)]
mod tests;

pub use core::Crossover;

use design::{gain, validate};

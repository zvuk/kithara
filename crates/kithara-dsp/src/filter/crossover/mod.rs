mod core;
mod design;

#[cfg(test)]
mod tests;

pub use core::Crossover;

use design::{coefficients, validate};

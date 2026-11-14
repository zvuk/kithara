mod core;
mod design;

#[cfg(test)]
#[path = "../../backend/crossover/tests.rs"]
mod tests;

pub use core::Crossover;

use design::{gain, validate};

//! Gapless trimmer — applies leading/trailing trim to a decoded PCM stream.

mod buffer;
mod core;
mod fade;
#[cfg(test)]
mod tests;

pub use core::{GaplessOutput, GaplessTrimmer};

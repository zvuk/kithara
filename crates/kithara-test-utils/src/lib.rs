#![allow(
    clippy::unwrap_used,
    reason = "test utility crate — unwraps are acceptable"
)]
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_lossless,
    reason = "test utility crate — numeric casts are acceptable for WAV generation"
)]
#![allow(
    clippy::missing_panics_doc,
    reason = "test utility crate — panic documentation not needed"
)]

#[cfg(test)]
extern crate self as kithara_test_utils;

pub mod hang;
pub mod mock;
pub mod probe;
pub mod test;

pub mod kithara {
    pub use kithara_test_macros::{Probe, fixture, hang_watchdog, mock, probe, test};
}

#[cfg(feature = "test")]
pub use test::*;

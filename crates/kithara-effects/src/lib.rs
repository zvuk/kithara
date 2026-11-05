#![forbid(unsafe_code)]
#![cfg_attr(all(rtsan, not(rtsan_standalone)), feature(sanitize))]

//! Deck and master audio effects.

mod dsp;
pub mod eq;
mod gain_db;
mod limiter;
pub mod node;

pub use gain_db::GainDb;
#[cfg(test)]
pub(crate) use kithara_test_utils::bufpool as test_pools;
pub use limiter::{LimiterConfig, LimiterError, PeakLimiter};
mod consts;

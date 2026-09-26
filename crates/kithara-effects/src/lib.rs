#![forbid(unsafe_code)]
#![cfg_attr(all(rtsan, not(rtsan_standalone)), feature(sanitize))]

//! Channel and master audio effects.

mod chain;
mod contract;
mod drain;
mod dsp;
pub mod eq;
mod gain_db;
mod limiter;
pub mod node;

pub use chain::{apply_effects, held_source_frames, reset_effects};
pub use contract::AudioEffect;
#[cfg(any(test, feature = "mock"))]
pub use contract::AudioEffectMock;
pub use drain::{EffectDrain, EffectDrainStep};
pub use gain_db::GainDb;
#[cfg(test)]
pub(crate) use kithara_test_utils::bufpool as test_pools;
pub use limiter::{LimiterConfig, LimiterError, PeakLimiter};
mod consts;

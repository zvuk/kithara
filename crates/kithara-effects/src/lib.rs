#![forbid(unsafe_code)]
#![cfg_attr(all(rtsan, not(rtsan_standalone)), feature(sanitize))]

//! Deck and master audio effects.

mod deck_effect;
mod dsp;
pub mod eq;
pub mod filter;
mod gain_db;
mod limiter;
pub mod node;

pub use deck_effect::{DeckEffect, FxParamChange, FxParamDescriptor, FxParamMapping, FxParamUnit};
pub use gain_db::GainDb;
#[cfg(test)]
pub(crate) use kithara_test_utils::bufpool as test_pools;
pub use limiter::{
    Ceiling, CeilingError, LimiterConfig, LimiterError, LookaheadMs, LookaheadMsError, PeakLimiter,
    ReleaseMs, ReleaseMsError,
};
mod consts;

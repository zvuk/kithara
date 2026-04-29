//! Symphonia-based audio decoder backend.
//!
//! Provides [`SymphoniaDecoder`], implementing both [`crate::traits::Decoder`]
//! (runtime) and [`crate::backend::Backend`] (capability + factory).
//!
//! # Direct Reader Creation (No Probe)
//!
//! When `ContainerFormat` is provided in config, the decoder creates the
//! appropriate format reader directly without probing. This is critical
//! for HLS streams where the container format is known.

pub(crate) mod adapter;
mod backend;
pub(crate) mod config;
pub(crate) mod decoder;
pub(crate) mod echain;
pub(crate) mod probe;

#[cfg(test)]
mod tests;

pub(crate) use self::decoder::SymphoniaDecoder;

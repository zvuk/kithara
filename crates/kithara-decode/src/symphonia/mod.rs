//! Symphonia-based audio decoder backend.
//!
//! Provides the [`Symphonia`] decoder that adapts `SymphoniaInner` to the
//! [`InnerDecoder`] trait used by the rest of the crate.
//!
//! # Direct Reader Creation (No Probe)
//!
//! When `ContainerFormat` is provided in config, the decoder creates the
//! appropriate format reader directly without probing. This is critical
//! for HLS streams where the container format is known.

pub(crate) mod adapter;
pub(crate) mod config;
mod decoder;
pub(crate) mod entry;
pub(crate) mod error_chain;
pub(crate) mod inner;
pub(crate) mod probe;

#[cfg(test)]
mod tests;

pub(crate) use self::{config::SymphoniaConfig, entry::create_from_boxed};

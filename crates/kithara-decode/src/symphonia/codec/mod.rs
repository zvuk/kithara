mod config;
mod core;
#[cfg(all(not(target_arch = "wasm32"), feature = "fdk-aac"))]
mod fdk;
#[cfg(feature = "opus")]
mod opus;
mod registry;
mod timing;

pub(crate) use core::SymphoniaCodec;

pub(crate) use config::SymphoniaConfig;
pub(super) use registry::get_probe;

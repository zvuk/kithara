#![forbid(unsafe_code)]

mod core;
#[cfg(not(target_arch = "wasm32"))]
mod disk;

pub use core::EvictConfig;
pub(crate) use core::LruIndex;

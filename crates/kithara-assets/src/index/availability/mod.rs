#![forbid(unsafe_code)]

mod core;
#[cfg(not(target_arch = "wasm32"))]
mod disk;

pub(crate) use core::{AvailabilityIndex, ScopedAvailabilityObserver};

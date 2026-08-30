#![forbid(unsafe_code)]

mod core;
#[cfg(not(target_arch = "wasm32"))]
mod disk;
mod retire;

pub(crate) use core::{ABSOLUTE_ROOT, AvailabilityIndex, ScopedAvailabilityObserver};

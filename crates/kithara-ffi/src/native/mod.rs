#[cfg(target_os = "android")]
pub(crate) mod android;
#[cfg(all(target_os = "android", feature = "test"))]
pub(crate) mod android_test;
pub(crate) mod bridge;
pub mod cipher;
pub(crate) mod config;
pub mod logging;
mod runtime;

pub(crate) use bridge::{event_bridge, item_bridge};
pub(crate) use runtime::FFI_RUNTIME;

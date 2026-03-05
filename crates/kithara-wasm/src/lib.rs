// WASM HLS player library entry point.

#[cfg(feature = "internal")]
pub mod internal;

#[cfg(target_arch = "wasm32")]
mod bindings;

#[cfg(target_arch = "wasm32")]
mod commands;
#[cfg(target_arch = "wasm32")]
mod js_channel;
#[cfg(target_arch = "wasm32")]
mod player;
#[cfg(target_arch = "wasm32")]
mod worker_entry;

#[cfg(target_arch = "wasm32")]
pub use bindings::build_info;

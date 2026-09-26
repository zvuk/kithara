#[cfg(not(any(feature = "gui", feature = "lib-only")))]
compile_error!("Enable at least one frontend feature: `gui` or `lib-only`");

#[cfg(feature = "gui")]
mod analysis;
mod baked;
#[cfg(feature = "gui")]
mod broadcast;
pub mod catalog;
pub mod config;
pub mod crossfade;
pub mod deck;
pub mod document;
#[cfg(feature = "gui")]
mod engine;
#[cfg(not(target_arch = "wasm32"))]
pub mod logging;
pub mod memory;
pub mod mix;
pub mod pools;
pub mod recording;
pub mod sources;
#[cfg(feature = "gui")]
pub mod state;
pub mod theme;
#[cfg(feature = "gui")]
mod wave_cache;
pub mod waveform;
#[cfg(all(target_arch = "wasm32", feature = "gui"))]
pub mod web;

pub use baked::secret;

#[cfg(feature = "gui")]
pub mod gui;

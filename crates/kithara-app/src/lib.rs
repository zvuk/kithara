#[cfg(not(any(feature = "gui", feature = "lib-only")))]
compile_error!("Enable at least one frontend feature: `gui` or `lib-only`");

mod analysis;
pub mod baked;
pub mod beatmatch;
mod broadcast;
pub mod catalog;
pub mod config;
pub mod crossfade;
pub mod deck;
pub mod mix;
pub mod sources;
pub mod state;
pub mod theme;
pub mod tracing_init;
mod wave_cache;
pub mod waveform;

#[cfg(feature = "gui")]
pub mod gui;

#[cfg(not(any(feature = "gui", feature = "lib-only")))]
compile_error!("Enable at least one frontend feature: `gui` or `lib-only`");

#[cfg(feature = "gui")]
mod analysis;
pub mod baked;
#[cfg(feature = "gui")]
mod broadcast;
pub mod catalog;
pub mod config;
pub mod crossfade;
pub mod deck;
pub mod mix;
pub mod pools;
pub mod recording;
pub mod sources;
#[cfg(feature = "gui")]
pub mod state;
pub mod theme;
pub mod tracing_init;
#[cfg(feature = "gui")]
mod wave_cache;
pub mod waveform;

#[cfg(feature = "gui")]
pub mod gui;

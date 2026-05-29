#[cfg(not(any(feature = "tui", feature = "gui", feature = "lib-only")))]
compile_error!("Enable at least one frontend feature: `tui`, `gui`, or `lib-only`");

pub mod baked;
pub mod config;
pub mod crossfade;
pub mod events;
pub mod frontend;
pub mod sources;
pub mod state;
pub mod theme;
pub mod tracing_init;

#[cfg(any(feature = "tui", feature = "gui"))]
mod track;

#[cfg(feature = "tui")]
pub mod tui;

#[cfg(feature = "gui")]
pub mod gui;

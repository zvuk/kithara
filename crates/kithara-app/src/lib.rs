#[cfg(not(any(feature = "tui", feature = "gui", feature = "lib-only")))]
compile_error!("Enable at least one frontend feature: `tui`, `gui`, or `lib-only`");

pub mod config;
pub mod crossfade;
pub mod drm;
pub mod events;
pub mod frontend;
pub mod sources;
pub mod theme;

#[cfg(feature = "tui")]
pub mod tui;

#[cfg(feature = "gui")]
pub mod gui;

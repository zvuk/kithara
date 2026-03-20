// Require at least one frontend feature.
#[cfg(not(any(feature = "tui", feature = "gui")))]
compile_error!("Enable at least one frontend feature: `tui` or `gui`");

pub mod config;
pub mod controls;
pub mod crossfade;
pub mod events;
pub mod frontend;
pub mod playlist;
pub mod theme;

#[cfg(feature = "tui")]
pub mod tui;

#[cfg(feature = "gui")]
pub mod gui;

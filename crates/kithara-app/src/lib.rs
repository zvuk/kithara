mod app;
pub mod controller;
pub mod crossfade;
pub mod events;
pub mod tui_runner;

pub use app::{AppError, AppResult, Mode, resolve_mode, run, run_gui_sync, track_name};

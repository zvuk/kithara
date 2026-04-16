use std::sync::Arc;

use kithara_queue::Queue;

use crate::config::AppConfig;

/// Boxed error type used by all frontends.
pub type FrontendError = Box<dyn std::error::Error + Send + Sync>;

/// UI frontend lifecycle contract.
///
/// Both TUI and GUI implement this trait. The `main()` entrypoint
/// dispatches to the appropriate frontend based on `--mode`.
///
/// Each frontend manages its own runtime (tokio for TUI, iced for GUI).
/// Shared resources like the HTTP downloader travel through
/// [`AppConfig`] so frontends don't duplicate constructor shapes.
pub trait Frontend: Sized {
    /// Create the frontend from config.
    ///
    /// # Errors
    /// Returns an error if frontend initialization fails.
    fn new(config: &AppConfig) -> Result<Self, FrontendError>;

    /// Initialize the frontend (set up terminal / window).
    ///
    /// # Errors
    /// Returns an error if the frontend cannot start.
    fn start(&mut self, queue: Arc<Queue>) -> Result<(), FrontendError>;

    /// Run the main event loop. Blocks until exit.
    ///
    /// # Errors
    /// Returns an error if the event loop fails.
    fn run_loop(&mut self, queue: Arc<Queue>) -> Result<(), FrontendError>;

    /// Clean up resources (restore terminal / close window).
    ///
    /// # Errors
    /// Returns an error if cleanup fails.
    fn shutdown(&mut self) -> Result<(), FrontendError>;
}

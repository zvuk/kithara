use std::{
    fs::OpenOptions,
    sync::{Arc, Mutex},
};

use iced::Size;
use kithara_queue::Queue;
use tracing::metadata::LevelFilter;
use tracing_subscriber::EnvFilter;

use super::{app::Kithara, update, view};
use crate::{
    config::AppConfig,
    frontend::{Frontend, FrontendError},
    theme::gui,
};

/// Initialize tracing for GUI-only mode (no CRLF writer needed).
///
/// Reads the filter from `RUST_LOG` (falling back to `INFO`). If
/// `KITHARA_LOG_FILE` is set, output goes to that path (append mode);
/// otherwise to stderr.
///
/// # Errors
/// Returns an error if tracing initialization fails.
pub fn init_tracing() -> Result<(), FrontendError> {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::default().add_directive(LevelFilter::INFO.into()));
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_line_number(false)
        .with_file(false);

    if let Some(path) = std::env::var_os("KITHARA_LOG_FILE") {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(Box::<dyn std::error::Error + Send + Sync>::from)?;
        builder
            .with_writer(Mutex::new(file))
            .with_ansi(false)
            .init();
    } else {
        builder.init();
    }
    Ok(())
}

/// GUI frontend using iced.
pub struct GuiFrontend {
    config: AppConfig,
    palette: gui::GuiPalette,
}

impl Frontend for GuiFrontend {
    fn new(config: &AppConfig) -> Result<Self, FrontendError> {
        Ok(Self {
            palette: config.palette.into(),
            config: config.clone(),
        })
    }

    fn run_loop(&mut self, queue: Arc<Queue>) -> Result<(), FrontendError> {
        /// Default window width in logical pixels.
        const WINDOW_WIDTH: f32 = 448.0;

        /// Default window height in logical pixels.
        const WINDOW_HEIGHT: f32 = 734.0;

        let palette = self.palette;
        let config = self.config.clone();

        iced::application(
            move || Kithara::new(Arc::clone(&queue), palette, &config),
            update::update,
            view::view,
        )
        .title("Kithara")
        .theme(Kithara::theme)
        .subscription(Kithara::subscription)
        .window_size(Size::new(WINDOW_WIDTH, WINDOW_HEIGHT))
        .run()?;

        Ok(())
    }

    fn shutdown(&mut self) -> Result<(), FrontendError> {
        // iced handles cleanup internally.
        Ok(())
    }

    fn start(&mut self, _queue: Arc<Queue>) -> Result<(), FrontendError> {
        // iced handles window setup internally.
        Ok(())
    }
}

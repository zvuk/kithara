use iced::Size;
use tracing::metadata::LevelFilter;
use tracing_subscriber::EnvFilter;

use super::{app::Kithara, update, view};
use crate::{
    config::AppConfig,
    controls::AppController,
    frontend::{Frontend, FrontendError},
    theme::gui,
};

/// Default window width in logical pixels.
const WINDOW_WIDTH: f32 = 448.0;

/// Default window height in logical pixels.
const WINDOW_HEIGHT: f32 = 734.0;

/// Initialize tracing for GUI-only mode (no CRLF writer needed).
///
/// # Errors
/// Returns an error if tracing initialization fails.
pub fn init_tracing() -> Result<(), FrontendError> {
    let filter = EnvFilter::default().add_directive(LevelFilter::INFO.into());
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_line_number(false)
        .with_file(false)
        .init();
    Ok(())
}

/// GUI frontend using iced.
pub struct GuiFrontend {
    palette: gui::GuiPalette,
    tracks: Vec<String>,
}

impl Frontend for GuiFrontend {
    fn new(config: &AppConfig) -> Result<Self, FrontendError> {
        Ok(Self {
            palette: config.palette.into(),
            tracks: config.tracks.clone(),
        })
    }

    fn start(&mut self, _controller: &mut AppController) -> Result<(), FrontendError> {
        // iced handles window setup internally.
        Ok(())
    }

    fn run_loop(&mut self, controller: &mut AppController) -> Result<(), FrontendError> {
        let player = controller.player().clone();
        let tracks = self.tracks.clone();
        let palette = self.palette;

        // iced owns the tokio runtime — this must NOT be called from within block_on().
        iced::application(
            move || Kithara::new(player.clone(), tracks.clone(), true, palette),
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
}

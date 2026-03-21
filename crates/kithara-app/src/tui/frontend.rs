use super::runner;
use crate::{
    config::AppConfig,
    controls::AppController,
    frontend::{Frontend, FrontendError},
    playlist::Playlist,
    theme::tui,
};

/// TUI frontend using ratatui.
pub struct TuiFrontend {
    palette: tui::TuiPalette,
    track_names: Vec<String>,
    urls: Vec<String>,
}

impl Frontend for TuiFrontend {
    fn new(config: &AppConfig) -> Result<Self, FrontendError> {
        let playlist = Playlist::new(config.tracks.clone());
        let track_names: Vec<String> = (0..playlist.len())
            .map(|i| playlist.track_name(i))
            .collect();

        Ok(Self {
            palette: config.palette.into(),
            track_names,
            urls: config.tracks.clone(),
        })
    }

    fn start(&mut self, _controller: &mut AppController) -> Result<(), FrontendError> {
        // Terminal setup happens in run_loop (UiSession::new).
        Ok(())
    }

    fn run_loop(&mut self, controller: &mut AppController) -> Result<(), FrontendError> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .max_blocking_threads(4)
            .enable_all()
            .build()?;

        let urls = self.urls.clone();
        let track_names = self.track_names.clone();
        let palette = self.palette;

        rt.block_on(runner::run_tui(controller, urls, track_names, palette))
    }

    fn shutdown(&mut self) -> Result<(), FrontendError> {
        // Terminal cleanup happens via UiSession/RawModeGuard Drop.
        Ok(())
    }
}

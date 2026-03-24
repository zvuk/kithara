use super::runner;
use crate::{
    config::AppConfig,
    controls::AppController,
    frontend::{Frontend, FrontendError},
    playlist::Playlist,
};

/// TUI frontend using ratatui.
pub struct TuiFrontend {
    config: AppConfig,
    track_names: Vec<String>,
}

impl Frontend for TuiFrontend {
    fn new(config: &AppConfig) -> Result<Self, FrontendError> {
        let playlist = Playlist::new(config.tracks.clone());
        let track_names: Vec<String> = (0..playlist.len())
            .map(|i| playlist.track_name(i))
            .collect();

        Ok(Self {
            config: config.clone(),
            track_names,
        })
    }

    fn start(&mut self, _controller: &mut AppController) -> Result<(), FrontendError> {
        // Terminal setup happens in run_loop (UiSession::new).
        Ok(())
    }

    fn run_loop(&mut self, controller: &mut AppController) -> Result<(), FrontendError> {
        const WORKER_THREADS: usize = 2;
        const MAX_BLOCKING_THREADS: usize = 4;
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(WORKER_THREADS)
            .max_blocking_threads(MAX_BLOCKING_THREADS)
            .enable_all()
            .build()?;

        let track_names = self.track_names.clone();

        rt.block_on(runner::run_tui(controller, &self.config, track_names))
    }

    fn shutdown(&mut self) -> Result<(), FrontendError> {
        // Terminal cleanup happens via UiSession/RawModeGuard Drop.
        Ok(())
    }
}

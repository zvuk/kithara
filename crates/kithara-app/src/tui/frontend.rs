use std::sync::Arc;

use kithara_queue::Queue;

use super::runner;
use crate::{
    config::AppConfig,
    frontend::{Frontend, FrontendError},
};

/// TUI frontend using ratatui.
pub struct TuiFrontend {
    config: AppConfig,
}

impl Frontend for TuiFrontend {
    fn new(config: &AppConfig) -> Result<Self, FrontendError> {
        Ok(Self {
            config: config.clone(),
        })
    }

    fn start(&mut self, _queue: Arc<Queue>) -> Result<(), FrontendError> {
        // Terminal setup happens in run_loop (UiSession::new).
        Ok(())
    }

    fn run_loop(&mut self, queue: Arc<Queue>) -> Result<(), FrontendError> {
        const WORKER_THREADS: usize = 2;
        const MAX_BLOCKING_THREADS: usize = 4;
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(WORKER_THREADS)
            .max_blocking_threads(MAX_BLOCKING_THREADS)
            .enable_all()
            .build()?;

        rt.block_on(runner::run_tui(queue, &self.config))
    }

    fn shutdown(&mut self) -> Result<(), FrontendError> {
        // Terminal cleanup happens via UiSession/RawModeGuard Drop.
        Ok(())
    }
}

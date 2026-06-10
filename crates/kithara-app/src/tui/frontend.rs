use std::sync::Arc;

use kithara::play::StretchControls;
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

    fn run_loop(
        &mut self,
        queue: Arc<Queue>,
        timestretch: Arc<StretchControls>,
    ) -> Result<(), FrontendError> {
        const WORKER_THREADS: usize = 2;
        const MAX_BLOCKING_THREADS: usize = 4;
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(WORKER_THREADS)
            .max_blocking_threads(MAX_BLOCKING_THREADS)
            .enable_all()
            .build()?;

        let result = rt.block_on(runner::run_tui(queue, timestretch, &self.config));

        self.config.shutdown.cancel();
        result
    }

    fn shutdown(&mut self) -> Result<(), FrontendError> {
        Ok(())
    }

    fn start(&mut self, _queue: Arc<Queue>) -> Result<(), FrontendError> {
        Ok(())
    }
}

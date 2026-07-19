use kithara_platform::{sync::Arc, tokio};

use super::runner;
use crate::{
    config::AppConfig,
    deck::DeckSet,
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

    /// The TUI drives a single deck; a multi-deck session is a GUI concept.
    fn run_loop(&mut self, decks: DeckSet) -> Result<(), FrontendError> {
        const WORKER_THREADS: usize = 2;
        const MAX_BLOCKING_THREADS: usize = 4;
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(WORKER_THREADS)
            .max_blocking_threads(MAX_BLOCKING_THREADS)
            .enable_all()
            .build()?;

        let deck = decks.decks().first().ok_or("no decks in the session")?;
        let result = rt.block_on(runner::run_tui(
            Arc::clone(&deck.queue),
            Arc::clone(&deck.timestretch),
            &self.config,
        ));

        self.config.shutdown.cancel();
        result
    }

    fn shutdown(&mut self) -> Result<(), FrontendError> {
        Ok(())
    }

    fn start(&mut self, _decks: &DeckSet) -> Result<(), FrontendError> {
        Ok(())
    }
}

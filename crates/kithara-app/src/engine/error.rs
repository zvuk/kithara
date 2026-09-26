use kithara::{platform::tokio::runtime::TryCurrentError, play::PlayError};

use crate::{document::AssembleError, wave_cache::AnalysisPersistenceError};

#[derive(Debug, derive_more::Display, derive_more::Error, derive_more::From)]
pub(crate) enum EngineError {
    #[display("document: {_0}")]
    Assemble(AssembleError),
    #[display("deck: {_0}")]
    Deck(PlayError),
    #[display("executor: {_0}")]
    NoExecutor(TryCurrentError),
    #[display("analysis persistence: {_0}")]
    Persistence(AnalysisPersistenceError),
    #[from(skip)]
    #[error(ignore)]
    #[display("the configuration lacks the {_0}")]
    Missing(&'static str),
    #[display("the engine thread panicked")]
    Panicked,
}

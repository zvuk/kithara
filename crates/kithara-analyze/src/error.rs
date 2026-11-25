use std::io;

use kithara::{bufpool::PoolError, platform::tokio::task::JoinError};

/// A batch could not be configured, executed or written.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RunError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Pool(#[from] PoolError),
    #[error(transparent)]
    Task(#[from] JoinError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

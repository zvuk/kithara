#![forbid(unsafe_code)]

use kithara_storage::StorageError;
use thiserror::Error;

/// Assets store errors.
#[derive(Debug, Error)]
pub enum CacheError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("storage error: {0}")]
    Storage(#[from] StorageError),
}

pub type CacheResult<T> = Result<T, CacheError>;

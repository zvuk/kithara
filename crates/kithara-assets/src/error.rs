#![forbid(unsafe_code)]

use kithara_storage::StorageError;
use thiserror::Error;

/// Assets store errors.
#[derive(Debug, Error)]
pub enum AssetsError {
    #[error("invalid resource key")]
    InvalidKey,

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("storage error: {0}")]
    Storage(#[from] StorageError),
}

pub type AssetsResult<T> = Result<T, AssetsError>;

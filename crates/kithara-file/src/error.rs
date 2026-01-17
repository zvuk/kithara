#![forbid(unsafe_code)]

use kithara_net::NetError;
use kithara_storage::StorageError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SourceError {
    #[error("Network error: {0}")]
    Net(#[from] NetError),

    #[error("Assets error: {0}")]
    Assets(#[from] kithara_assets::AssetsError),

    #[error("Storage error: {0}")]
    Storage(#[from] StorageError),
}

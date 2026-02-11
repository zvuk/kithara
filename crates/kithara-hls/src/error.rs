#![forbid(unsafe_code)]

use thiserror::Error;

/// HLS orchestration errors.
#[derive(Debug, Error)]
pub enum HlsError {
    #[error("Network error: {0}")]
    Net(#[from] kithara_net::NetError),

    #[error("Assets error: {0}")]
    Assets(#[from] kithara_assets::AssetsError),

    #[error("Storage error: {0}")]
    Storage(#[from] kithara_storage::StorageError),

    #[error("Writer error: {0}")]
    Writer(#[from] kithara_stream::WriterError),

    #[error("Playlist parsing error: {0}")]
    PlaylistParse(String),

    #[error("Variant not found: {0}")]
    VariantNotFound(String),

    #[error("Segment not found: {0}")]
    SegmentNotFound(String),

    #[error("Key processing failed: {0}")]
    KeyProcessing(String),

    #[error("Invalid URL: {0}")]
    InvalidUrl(String),

    #[error("Cancelled")]
    Cancelled,
}

pub type HlsResult<T> = Result<T, HlsError>;

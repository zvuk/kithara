#![forbid(unsafe_code)]

use kithara_stream::SourceError;
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

    #[error("Timeout: {0}")]
    Timeout(String),
}

impl From<HlsError> for SourceError {
    fn from(err: HlsError) -> Self {
        match err {
            HlsError::Net(e) => Self::Net(e),
            HlsError::Storage(e) => Self::Storage(e),
            HlsError::Assets(e) => Self::other(e),
            HlsError::PlaylistParse(s) => Self::PlaylistParse(s),
            HlsError::VariantNotFound(s) => Self::VariantNotFound(s),
            HlsError::SegmentNotFound(s) => Self::SegmentNotFound(s),
            HlsError::KeyProcessing(s) => Self::KeyProcessing(s),
            HlsError::InvalidUrl(s) => Self::InvalidUrl(s),
            HlsError::Cancelled => Self::Cancelled,
            HlsError::Timeout(s) => Self::Timeout(s),
        }
    }
}

pub type HlsResult<T> = Result<T, HlsError>;

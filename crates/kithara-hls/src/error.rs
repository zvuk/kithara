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

    #[error("Playlist parsing error: {0}")]
    PlaylistParse(String),

    #[error("Variant not found: {0}")]
    VariantNotFound(String),

    #[error("Segment not found: {0}")]
    SegmentNotFound(String),

    #[error("No suitable variant found")]
    NoSuitableVariant,

    #[error("Key processing failed: {0}")]
    KeyProcessing(String),

    #[error("ABR error: {0}")]
    Abr(String),

    #[error("Offline mode: resource not cached")]
    OfflineMiss,

    #[error("Invalid URL: {0}")]
    InvalidUrl(String),

    #[error("not implemented")]
    Unimplemented,

    #[error("Driver error: {0}")]
    Driver(String),
}

pub type HlsResult<T> = Result<T, HlsError>;

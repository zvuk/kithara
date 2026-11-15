#![forbid(unsafe_code)]

use kithara_net::NetError;
use kithara_storage::StorageError;
use kithara_stream::SourceError as StreamSourceError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SourceError {
    /// A remote file has no injected downloader.
    #[error("a remote file source requires a Downloader")]
    NoDownloader,

    #[error("Network error: {0}")]
    Net(#[from] NetError),

    #[error("Assets error: {0}")]
    Assets(#[from] kithara_assets::AssetsError),

    #[error("Storage error: {0}")]
    Storage(#[from] StorageError),

    #[error("Invalid path: {0}")]
    InvalidPath(String),

    #[error("unexpected asset acquisition state")]
    UnexpectedAcquisitionState,

    #[error("another writer of this store is settling the resource")]
    WriterSettling,
}

impl From<SourceError> for StreamSourceError {
    fn from(err: SourceError) -> Self {
        match err {
            SourceError::Net(e) => Self::Net(e),
            SourceError::Storage(e) => Self::Storage(e),
            SourceError::Assets(e) => Self::other(e),
            SourceError::InvalidPath(s) => Self::InvalidPath(s),
            SourceError::NoDownloader
            | SourceError::UnexpectedAcquisitionState
            | SourceError::WriterSettling => Self::other(err),
        }
    }
}

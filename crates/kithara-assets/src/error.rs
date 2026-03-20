#![forbid(unsafe_code)]

use std::io;

use kithara_storage::StorageError;
use thiserror::Error;

/// Assets store errors.
#[derive(Debug, Error)]
pub enum AssetsError {
    #[error("invalid resource key")]
    InvalidKey,

    #[error("io error: {0}")]
    Io(#[from] io::Error),

    #[error("serialization error: {0}")]
    Serialization(#[from] postcard::Error),

    #[error("storage error: {0}")]
    Storage(#[from] StorageError),

    #[error("URL canonicalization failed: {0}")]
    Canonicalization(String),

    #[error("Invalid URL: {0}")]
    InvalidUrl(String),

    #[error("URL is missing required component: {0}")]
    MissingComponent(String),
}

pub type AssetsResult<T> = Result<T, AssetsError>;

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    #[case::invalid_key(AssetsError::InvalidKey, "invalid resource key")]
    #[case::canonicalization(AssetsError::Canonicalization("path error".into()), "URL canonicalization failed: path error")]
    #[case::invalid_url(AssetsError::InvalidUrl("bad://url".into()), "Invalid URL: bad://url")]
    #[case::missing_component(AssetsError::MissingComponent("host".into()), "URL is missing required component: host")]
    fn test_error_display(#[case] error: AssetsError, #[case] expected: &str) {
        assert_eq!(error.to_string(), expected);
    }

    #[kithara::test]
    fn test_serialization_error_display() {
        let err = AssetsError::Serialization(postcard::Error::DeserializeUnexpectedEnd);
        assert!(err.to_string().starts_with("serialization error:"));
    }

    #[kithara::test]
    fn test_serialization_error_from() {
        let postcard_err = postcard::Error::DeserializeUnexpectedEnd;
        let err: AssetsError = postcard_err.into();
        assert!(matches!(err, AssetsError::Serialization(_)));
    }

    #[kithara::test]
    fn test_assets_error_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<AssetsError>();
    }
}

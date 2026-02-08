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

    #[error("bincode error: {0}")]
    Bincode(#[from] bincode::error::EncodeError),

    #[error("bincode decode error: {0}")]
    BincodeDecode(#[from] bincode::error::DecodeError),

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
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::invalid_key(AssetsError::InvalidKey, "invalid resource key")]
    #[case::canonicalization(AssetsError::Canonicalization("path error".into()), "URL canonicalization failed: path error")]
    #[case::invalid_url(AssetsError::InvalidUrl("bad://url".into()), "Invalid URL: bad://url")]
    #[case::missing_component(AssetsError::MissingComponent("host".into()), "URL is missing required component: host")]
    #[test]
    fn test_error_display(#[case] error: AssetsError, #[case] expected: &str) {
        assert_eq!(error.to_string(), expected);
    }

    #[test]
    fn test_bincode_error_display() {
        let err = AssetsError::Bincode(bincode::error::EncodeError::UnexpectedEnd);
        assert!(err.to_string().starts_with("bincode error:"));
    }

    #[test]
    fn test_bincode_encode_error_from() {
        let encode_err = bincode::error::EncodeError::UnexpectedEnd;
        let err: AssetsError = encode_err.into();
        assert!(matches!(err, AssetsError::Bincode(_)));
    }

    #[test]
    fn test_bincode_decode_error_display() {
        let err = AssetsError::BincodeDecode(bincode::error::DecodeError::UnexpectedEnd {
            additional: 4,
        });
        let msg = err.to_string();
        assert!(msg.starts_with("bincode decode error:"));
    }

    #[test]
    fn test_bincode_decode_error_from() {
        let decode_err = bincode::error::DecodeError::UnexpectedEnd { additional: 8 };
        let err: AssetsError = decode_err.into();
        assert!(matches!(err, AssetsError::BincodeDecode(_)));
    }

    #[test]
    fn test_assets_error_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<AssetsError>();
    }
}

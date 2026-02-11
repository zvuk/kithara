#![forbid(unsafe_code)]

use thiserror::Error;

/// DRM decryption errors.
#[derive(Debug, Error)]
pub enum DrmError {
    #[error("AES-128-CBC decryption failed: {0}")]
    DecryptFailed(String),

    #[error("Invalid key length: expected 16 bytes, got {0}")]
    InvalidKeyLength(usize),

    #[error("Invalid IV length: expected 16 bytes, got {0}")]
    InvalidIvLength(usize),

    #[error("Key processing failed: {0}")]
    KeyProcessing(String),
}

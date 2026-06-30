#![forbid(unsafe_code)]

use std::fmt;

/// AES-128-CBC decryption context.
///
/// Carried as a per-acquire `ProcessCtx` trait object
/// when decrypting a resource on commit.
#[derive(Clone, Default, Hash, PartialEq, Eq)]
pub struct DecryptContext {
    /// Initialization vector (16 bytes).
    pub iv: [u8; Self::IV_LEN],
    /// AES-128 key (16 bytes).
    pub key: [u8; Self::KEY_LEN_128],
}

impl fmt::Debug for DecryptContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DecryptContext")
            .field("key", &"<redacted>")
            .field("iv", &"<redacted>")
            .finish()
    }
}

impl DecryptContext {
    /// AES initialization vector length in bytes.
    const IV_LEN: usize = 16;

    /// AES-128 key length in bytes.
    const KEY_LEN_128: usize = 16;

    /// Create a new decryption context.
    #[must_use]
    pub fn new(key: [u8; Self::KEY_LEN_128], iv: [u8; Self::IV_LEN]) -> Self {
        Self { iv, key }
    }
}

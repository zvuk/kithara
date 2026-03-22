#![forbid(unsafe_code)]

//! Decryption context for `ProcessingAssets`.

/// AES-128 key length in bytes.
const KEY_LEN_128: usize = 16;

/// AES initialization vector length in bytes.
const IV_LEN: usize = 16;

/// AES-128-CBC decryption context.
///
/// Passed as the `Ctx` type parameter to `ProcessingAssets<A, DecryptContext>`.
/// When `Some(ctx)` is provided to `open_resource_with_ctx`, the resource
/// will be decrypted on commit. When `None`, data passes through unchanged
/// (used for playlists, keys, init segments).
#[derive(Clone, Debug, Default, Hash, PartialEq, Eq)]
pub struct DecryptContext {
    /// AES-128 key (16 bytes).
    pub key: [u8; KEY_LEN_128],
    /// Initialization vector (16 bytes).
    pub iv: [u8; IV_LEN],
}

impl DecryptContext {
    /// Create a new decryption context.
    #[must_use]
    pub fn new(key: [u8; KEY_LEN_128], iv: [u8; IV_LEN]) -> Self {
        Self { key, iv }
    }
}

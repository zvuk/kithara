#![forbid(unsafe_code)]

//! DRM decryption support for kithara.
//!
//! Provides AES-128-CBC decryption, wired into the asset store as a
//! per-acquire `ProcessCtx` trait object.
//!
//! # Key processing
//!
//! Supports a `KeyProcessor` callback for in-house DRM where the key
//! fetched from the server is itself encrypted and needs to be unwrapped
//! with an application-embedded key.

mod cipher;
mod context;
mod decrypt;
mod error;
mod registry;

#[cfg(test)]
mod tests;

pub use cipher::UniqueBinaryCipher;
pub use context::DecryptContext;
pub use decrypt::aes128_cbc_process_chunk;
pub use error::DrmError;
pub use registry::{
    DomainMatcher, KeyProcessResult, KeyProcessor, KeyProcessorRegistry, KeyProcessorRule,
    KeyRequest, KeyRequestFactory,
};

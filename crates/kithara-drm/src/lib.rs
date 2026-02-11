#![forbid(unsafe_code)]

//! DRM decryption support for kithara.
//!
//! Provides AES-128-CBC decryption usable as a `ProcessChunkFn` callback
//! in `kithara-assets` `ProcessingAssets` decorator. Designed to be
//! protocol-agnostic (works with HLS, DASH, etc.).
//!
//! # Key processing
//!
//! Supports a `KeyProcessor` callback for in-house DRM where the key
//! fetched from the server is itself encrypted and needs to be unwrapped
//! with an application-embedded key.

mod context;
mod decrypt;
mod error;

pub use context::DecryptContext;
pub use decrypt::aes128_cbc_process_chunk;
pub use error::DrmError;

//! Test fixtures for HLS integration tests
//!
//! This module provides reusable test infrastructure including:
//! - HTTP test servers for HLS content (in-process on native, remote on WASM)
//! - Asset store helpers
//! - Encryption/decryption utilities
//! - ABR testing infrastructure

pub mod abr;
pub mod assets;
pub mod crypto;
pub mod scalable_server;
pub mod server;

// Re-export commonly used types
pub use assets::*;
#[cfg(not(target_arch = "wasm32"))]
pub use crypto::*;
#[cfg(target_arch = "wasm32")]
pub use crypto::{aes128_iv, aes128_plaintext_segment};
// Common types
use kithara::hls::HlsError;
pub use scalable_server::*;
pub use server::*;

pub type HlsResult<T> = Result<T, HlsError>;

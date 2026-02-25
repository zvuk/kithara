//! Test fixtures for HLS integration tests
//!
//! This module provides reusable test infrastructure including:
//! - HTTP test servers for HLS content (in-process on native, remote on WASM)
//! - Asset store helpers
//! - Encryption/decryption utilities
//! - ABR testing infrastructure

pub(crate) mod abr;
pub(crate) mod assets;
pub(crate) mod crypto;
pub(crate) mod scalable_server;
pub(crate) mod server;

// Re-export commonly used types
pub(crate) use assets::*;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use crypto::*;
#[cfg(target_arch = "wasm32")]
pub(crate) use crypto::{aes128_iv, aes128_plaintext_segment};
// Common types
use kithara::hls::HlsError;
pub(crate) use scalable_server::*;
pub(crate) use server::*;

pub(crate) type HlsResult<T> = Result<T, HlsError>;

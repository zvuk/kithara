//! Test fixtures for HLS integration tests
//!
//! This module provides reusable test infrastructure including:
//! - HTTP test servers for HLS content
//! - Asset store helpers
//! - Encryption/decryption utilities
//! - ABR testing infrastructure

pub mod abr;
pub mod assets;
pub mod crypto;
pub mod server;

// Re-export commonly used types
pub use abr::*;
pub use assets::*;
pub use crypto::*;
pub use server::*;

// Common types
use kithara_hls::HlsError;

pub type HlsResult<T> = Result<T, HlsError>;

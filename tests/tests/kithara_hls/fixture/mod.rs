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
pub mod scalable_server;
pub mod server;

// Re-export commonly used types
pub use assets::*;
pub use crypto::*;
// Common types
use kithara_hls::HlsError;
pub use scalable_server::*;
pub use server::*;

pub type HlsResult<T> = Result<T, HlsError>;

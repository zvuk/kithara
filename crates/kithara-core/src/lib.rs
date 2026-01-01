#![forbid(unsafe_code)]

//! # kithara-core
//!
//! Minimal core types shared across kithara crates.
//!
//! ## What belongs here
//! - Identity types (`AssetId`, `ResourceHash`) with URL canonicalization
//! - Core error types used by multiple crates
//! - Basic utilities that all other crates depend on
//!
//! ## What must NOT be here
//! - **No settings/options** - CacheOptions belongs in kithara-cache, HlsOptions belongs in kithara-hls
//! - No networking, I/O, or async code
//! - No dependencies on tokio, reqwest, symphonia, or other heavy libraries
//! - No crate-specific logic (HLS, caching, decoding, etc.)
//!
//! This crate intentionally stays minimal to avoid becoming a dumping ground for shared code.

// Compile-time guard: settings types must not be added to kithara-core
#[cfg(any(feature = "cache-options", feature = "hls-options"))]
compile_error!("Settings/options belong in their respective crates, not kithara-core");

pub mod asset_id;
pub mod canonicalization;
pub mod errors;
pub mod resource_hash;

// Re-export public types
pub use asset_id::AssetId;
pub use errors::{CoreError, CoreResult};
pub use resource_hash::ResourceHash;

// Re-export internal functions for use within the crate
pub(crate) use canonicalization::{canonicalize_for_asset, canonicalize_for_resource};

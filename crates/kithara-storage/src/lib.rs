#![forbid(unsafe_code)]

//! `kithara-storage`
//!
//! Storage primitives for Kithara.
//!
//! This crate provides two resource kinds:
//! - [`StreamingResource`]: random-access writes + waitable ranges for streaming playback.
//! - [`AtomicResource`]: atomic whole-file reads/writes for small metadata blobs (index, playlists, keys).
//!
//! Shared contracts:
//! - [`Resource`]: whole-object `read` / `write` + lifecycle (`commit`/`fail`).
//! - [`StreamingResourceExt`]: range-based `read_at` / `write_at` + `wait_range`.
//! - [`AtomicResourceExt`]: marker for atomic small-object resources.
//!
//! Higher-level concerns (trees of resources, eviction, leases, network orchestration) belong to
//! other crates.

mod atomic;
mod error;
mod resource;
mod streaming;

pub use atomic::{AtomicOptions, AtomicResource};
pub use error::{StorageError, StorageResult};
pub use resource::{AtomicResourceExt, Resource, StreamingResourceExt, WaitOutcome};
pub use streaming::{DiskOptions, ResourceStatus, StreamingResource};

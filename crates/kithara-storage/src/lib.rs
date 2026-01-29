#![forbid(unsafe_code)]

//! `kithara-storage`
//!
//! Storage primitives for Kithara.
//!
//! Provides a unified `StorageResource` backed by `mmap-io` with:
//! - Random-access `read_at`/`write_at` (streaming use-case)
//! - Blocking `wait_range` via `Condvar`
//! - Convenience `read_into`/`write_all` (atomic use-case)

mod error;
mod resource;

pub use error::{StorageError, StorageResult};
pub use resource::{ResourceExt, ResourceStatus, StorageOptions, StorageResource, WaitOutcome};

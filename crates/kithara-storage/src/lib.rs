#![forbid(unsafe_code)]

//! `kithara-storage`
//!
//! Storage primitives for Kithara.
//!
//! Provides a unified `StorageResource` backed by `mmap-io` with:
//! - Random-access `read_at`/`write_at` (streaming use-case)
//! - Blocking `wait_range` via `Condvar`
//! - Convenience `read_into`/`write_all` (atomic use-case)
//!
//! Also provides [`MemoryResource`] — an in-memory alternative for platforms
//! without filesystem access (e.g. WASM).

mod error;
mod memory;
mod resource;

pub use error::{StorageError, StorageResult};
pub use memory::MemoryResource;
pub use resource::{
    OpenMode, ResourceExt, ResourceStatus, StorageOptions, StorageResource, WaitOutcome,
};

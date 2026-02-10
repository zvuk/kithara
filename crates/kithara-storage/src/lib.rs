#![forbid(unsafe_code)]

//! `kithara-storage`
//!
//! Storage primitives for Kithara.
//!
//! Provides a generic [`Resource<D>`] parameterized by a [`Driver`]:
//! - [`MmapResource`] — mmap-backed (filesystem), with lock-free fast path.
//! - [`MemResource`] — in-memory `Vec<u8>` (WASM).
//!
//! The consumer-facing trait is [`ResourceExt`].

mod atomic;
mod coverage;
mod driver;
mod error;
mod memory;
mod mmap;
mod resource;

pub use atomic::{Atomic, AtomicMem, AtomicMmap};
pub use coverage::{Coverage, MemCoverage};
pub use driver::{Driver, DriverState, Resource};
pub use error::{StorageError, StorageResult};
pub use memory::{MemDriver, MemOptions, MemResource};
pub use mmap::{MmapDriver, MmapOptions, MmapResource};
#[cfg(any(test, feature = "test-utils"))]
pub use resource::ResourceMock;
pub use resource::{OpenMode, ResourceExt, ResourceStatus, WaitOutcome};

// Backward compatibility aliases.
#[deprecated(note = "renamed to MmapResource")]
pub type StorageResource = MmapResource;
#[deprecated(note = "renamed to MmapOptions")]
pub type StorageOptions = MmapOptions;
#[deprecated(note = "renamed to MemResource")]
pub type MemoryResource = MemResource;

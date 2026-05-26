#![forbid(unsafe_code)]

//! `kithara-storage`
//!
//! Storage primitives for Kithara.
//!
//! Provides a generic [`Resource<D>`] parameterized by a [`Driver`]:
//! - [`MmapResource`] — mmap-backed (filesystem), with lock-free fast path.
//! - [`MemResource`] — in-memory `Vec<u8>` (WASM).
//!
//! [`StorageResource`] is a unified enum combining both backends.
//!
//! The consumer-facing trait is [`ResourceExt`].

mod backend;
mod decorator;
mod error;
mod resource;
mod unified;

#[cfg(any(test, feature = "mock"))]
pub mod mock;

pub use backend::{
    AvailabilityObserver, Driver, DriverIo, MemDriver, MemOptions, MemResource, Resource,
};
#[cfg(not(target_arch = "wasm32"))]
pub use backend::{MmapDriver, MmapOptions, MmapResource};
pub use decorator::{Atomic, AtomicChunked, OpenIntent};
pub use error::{StorageError, StorageResult};
pub use resource::{OpenMode, ResourceExt, ResourceStatus, WaitOutcome};
pub use unified::StorageResource;

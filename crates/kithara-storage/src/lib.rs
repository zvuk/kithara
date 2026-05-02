#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::ignored_unit_patterns, clippy::allow_attributes))]

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

#[cfg(feature = "internal")]
pub mod internal;
#[cfg(any(test, feature = "test-utils"))]
pub mod mock;

pub use backend::{
    AvailabilityObserver, Driver, DriverIo, MemDriver, MemOptions, MemResource, Resource,
};
#[cfg(not(target_arch = "wasm32"))]
pub use backend::{MmapDriver, MmapOptions, MmapResource};
#[cfg(not(target_arch = "wasm32"))]
pub use decorator::AtomicMmap;
pub use decorator::{Atomic, AtomicChunked, OpenIntent};
pub use error::{StorageError, StorageResult};
pub use resource::{OpenMode, ResourceExt, ResourceStatus, WaitOutcome};
pub use unified::StorageResource;

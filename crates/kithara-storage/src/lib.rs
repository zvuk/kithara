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

mod atomic;
mod coverage;
mod driver;
mod error;
mod memory;
#[cfg(not(target_arch = "wasm32"))]
mod mmap;
mod resource;
mod unified;

pub use atomic::Atomic;
#[cfg(not(target_arch = "wasm32"))]
pub use atomic::AtomicMmap;
pub use coverage::{Coverage, MemCoverage};
pub use driver::{Driver, DriverIo, Resource};
pub use error::{StorageError, StorageResult};
pub use memory::{MemDriver, MemOptions, MemResource};
#[cfg(not(target_arch = "wasm32"))]
pub use mmap::{MmapDriver, MmapOptions, MmapResource};
#[cfg(any(test, feature = "test-utils"))]
pub mod mock;

pub use resource::{OpenMode, ResourceExt, ResourceStatus, WaitOutcome};
pub use unified::StorageResource;

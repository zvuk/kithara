#![forbid(unsafe_code)]

//! `kithara-storage`
//!
//! Storage primitives for Kithara.
//!
//! Provides a phantom-typestate [`Resource<S, D>`] parameterized by a phase `S`
//! and a [`Driver`] `D`:
//! - [`ResourceWriter`] (`Resource<Active, D>`) — single-owner writeable handle.
//! - [`Resource<Committed, D>`] — sealed, read-final handle.
//! - [`ResourceReader`] (`Resource<Reader, D>`) — cloneable read-only view.
//!
//! Backends: [`MmapResource`] (mmap, filesystem) and [`MemResource`]
//! (in-memory, WASM). [`StorageResource`] is a unified enum combining both.
//!
//! The consumer-facing read API is the sealed [`ResourceRead`] trait.

mod backend;
mod decorator;
mod error;
mod resource;
mod unified;

#[cfg(any(test, feature = "mock"))]
pub mod mock;

pub use backend::{
    Active, AvailabilityObserver, Committed, Driver, DriverIo, MemDriver, MemOptions, MemResource,
    Reader, Resource, ResourcePhase, ResourceRead, ResourceReader, ResourceWriter,
};
#[cfg(not(target_arch = "wasm32"))]
pub use backend::{MmapDriver, MmapOptions, MmapResource};
pub use decorator::{Atomic, AtomicChunked, OpenIntent};
pub use error::{StorageError, StorageResult};
pub use resource::{OpenMode, ResourceStatus, WaitOutcome};
pub use unified::StorageResource;

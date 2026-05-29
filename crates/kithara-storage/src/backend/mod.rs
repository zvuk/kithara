#![forbid(unsafe_code)]

//! Backend layer: driver contracts + concrete drivers + the generic
//! `ResourceCore<D>` state machine and its typed `Resource<S, D>` handles.

pub(crate) mod resource;
pub(crate) mod traits;

pub(crate) mod memory;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod mmap;

pub use memory::{MemDriver, MemOptions, MemResource};
#[cfg(not(target_arch = "wasm32"))]
pub use mmap::{MmapDriver, MmapOptions, MmapResource};
pub use resource::{
    Active, Committed, Reader, Resource, ResourcePhase, ResourceRead, ResourceReader,
    ResourceWriter,
};
#[cfg(any(test, feature = "probe"))]
pub use traits::DriverIoMock;
pub use traits::{AvailabilityObserver, Driver, DriverIo};

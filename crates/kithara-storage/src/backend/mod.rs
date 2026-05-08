#![forbid(unsafe_code)]

//! Backend layer: driver contracts + concrete drivers + the generic
//! [`Resource<D>`] state machine.

pub(crate) mod resource;
pub(crate) mod traits;

pub(crate) mod memory;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod mmap;

pub use memory::{MemDriver, MemOptions, MemResource};
#[cfg(not(target_arch = "wasm32"))]
pub use mmap::{MmapDriver, MmapOptions, MmapResource};
pub use resource::Resource;
#[cfg(any(test, feature = "test-utils"))]
pub use traits::DriverIoMock;
pub use traits::{AvailabilityObserver, Driver, DriverIo};

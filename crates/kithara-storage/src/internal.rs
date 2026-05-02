#![forbid(unsafe_code)]

#[cfg(not(target_arch = "wasm32"))]
pub use crate::backend::{MmapDriver, MmapOptions, MmapResource};
#[cfg(not(target_arch = "wasm32"))]
pub use crate::decorator::AtomicMmap;
pub use crate::{
    backend::{Driver, DriverIo, MemDriver, MemOptions, MemResource, Resource},
    decorator::Atomic,
    error::{StorageError, StorageResult},
    resource::{OpenMode, ResourceExt, ResourceStatus, WaitOutcome},
    unified::StorageResource,
};

#![forbid(unsafe_code)]

#[cfg(not(target_arch = "wasm32"))]
pub use crate::atomic::AtomicMmap;
#[cfg(not(target_arch = "wasm32"))]
pub use crate::mmap::{MmapDriver, MmapOptions, MmapResource};
pub use crate::{
    atomic::Atomic,
    driver::{Driver, DriverIo, Resource},
    error::{StorageError, StorageResult},
    memory::{MemDriver, MemOptions, MemResource},
    resource::{OpenMode, ResourceExt, ResourceStatus, WaitOutcome},
    unified::StorageResource,
};

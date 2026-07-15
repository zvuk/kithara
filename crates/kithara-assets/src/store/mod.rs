mod builder;
mod chain;
mod handle;

pub use builder::{AssetStoreBuilder, StorageBackend, StoreOptions};
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use chain::DiskStore;
pub use chain::{AssetReader, AssetWriter, ResourceAcquisition};
pub(crate) use chain::{MemStore, OnInvalidatedFn};
pub use handle::AssetStore;

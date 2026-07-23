use kithara_platform::sync::Arc;

#[cfg(not(target_arch = "wasm32"))]
use crate::backend::DiskAssetStore;
use crate::{
    backend::MemAssetStore,
    decorator::{
        CachedAssets, CachedReader, CachedWriter, EvictAssets, LeaseAssets, LeaseGuard,
        LeaseReader, LeaseWriter, ProcessedReader, ProcessedWriter, ProcessingAssets,
    },
    layout::ResourceKey,
    resource::{AcquisitionResult, BaseReader, BaseWriter},
};

/// Hook fired when the cache volatile-displaces a resource.
pub(crate) type OnInvalidatedFn = Arc<dyn Fn(&ResourceKey) + Send + Sync>;

/// Fully decorated disk store chain. Processing travels per-acquire as a
/// [`crate::ProcessCtx`], so the chain is not generic over context.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) type DiskStore =
    LeaseAssets<CachedAssets<ProcessingAssets<EvictAssets<DiskAssetStore>>>>;

/// Pending (writer) handle returned by the `Pending` arm of
/// [`super::AssetStore::acquire_resource`]. Owns the streaming write + decrypt-on-commit
/// capability; consumes itself on `commit` into an [`AssetReader`].
pub type AssetWriter = LeaseWriter<CachedWriter<ProcessedWriter<BaseWriter>>, LeaseGuard>;

/// Ready (reader) handle returned by [`super::AssetStore::open_resource`] and the
/// `Ready` arm of [`super::AssetStore::acquire_resource`]. Cheap to clone.
pub type AssetReader = LeaseReader<CachedReader<ProcessedReader<BaseReader>>, LeaseGuard>;

/// Phase-typed acquisition outcome returned by
/// [`super::AssetStore::acquire_resource`]: a `Pending` [`AssetWriter`] to stream and
/// commit, or a `Ready` [`AssetReader`] when the resource is already committed.
pub type ResourceAcquisition = AcquisitionResult<AssetWriter, AssetReader>;

pub(crate) type MemStore = LeaseAssets<CachedAssets<ProcessingAssets<EvictAssets<MemAssetStore>>>>;

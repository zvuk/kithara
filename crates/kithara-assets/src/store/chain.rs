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
    store::AssetStoreConfig,
};

/// Hook fired when the cache volatile-displaces a resource.
pub(crate) type OnInvalidatedFn = Arc<dyn Fn(&ResourceKey) + Send + Sync>;

/// Fully decorated store chain. Processing travels per-acquire as a
/// [`crate::ProcessCtx`], so the chain is not generic over context.
pub(crate) type StoreChain<A, S> =
    LeaseAssets<CachedAssets<ProcessingAssets<EvictAssets<A, S>, S>, Arc<AssetStoreConfig<S>>>>;

#[cfg(not(target_arch = "wasm32"))]
pub(crate) type DiskStore<S> = StoreChain<DiskAssetStore, S>;

/// Pending (writer) handle returned by the `Pending` arm of
/// [`super::AssetStore::acquire_resource`]. Owns the streaming write + decrypt-on-commit
/// capability; consumes itself on `commit` into an [`AssetReader`].
pub type AssetWriter<S> = LeaseWriter<CachedWriter<ProcessedWriter<BaseWriter, S>>, LeaseGuard>;

/// Ready (reader) handle returned by [`super::AssetStore::open_resource`] and the
/// `Ready` arm of [`super::AssetStore::acquire_resource`]. Cheap to clone.
pub type AssetReader<S> = LeaseReader<CachedReader<ProcessedReader<BaseReader, S>>, LeaseGuard>;

/// Phase-typed acquisition outcome returned by
/// [`super::AssetStore::acquire_resource`]: a `Pending` [`AssetWriter`] to stream and
/// commit, a `Ready` [`AssetReader`] when the resource is already committed, or a
/// `Joined` [`AssetReader`] over another party's in-flight writer.
pub type ResourceAcquisition<S> = AcquisitionResult<AssetWriter<S>, AssetReader<S>>;

pub(crate) type MemStore<S> = StoreChain<MemAssetStore<S>, S>;

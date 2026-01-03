#![forbid(unsafe_code)]

use std::fmt;
use std::ops::Range;

use async_trait::async_trait;
use bytes::Bytes;

use kithara_storage::{
    Resource, StorageError, StreamingResource, StreamingResourceExt, WaitOutcome,
};

use crate::lease::LeaseGuard;

/// A resource handle returned by `kithara-assets` that automatically pins its `asset_root`.
///
/// This is a single generic decorator that can wrap both:
/// - `kithara_storage::AtomicResource`
/// - `kithara_storage::StreamingResource`
///
/// Pinning is keyed by `asset_root` (not per file). Dropping this handle releases the pin.
///
/// The wrapper implements `kithara_storage::Resource` by delegating to the inner resource.
/// For streaming-specific APIs it also implements `StreamingResourceExt` for
/// `AssetResource<StreamingResource>`.
pub struct AssetResource<R> {
    pub(crate) inner: R,
    pub(crate) _lease: LeaseGuard,
}

impl<R> AssetResource<R> {
    pub(crate) fn new(inner: R, lease: LeaseGuard) -> Self {
        Self {
            inner,
            _lease: lease,
        }
    }

    /// Borrow the inner resource.
    pub fn inner(&self) -> &R {
        &self.inner
    }

    /// Consume the wrapper and return the inner resource.
    ///
    /// Note: this also drops the lease guard, releasing the pin.
    pub fn into_inner(self) -> R {
        self.inner
    }
}

impl<R> fmt::Debug for AssetResource<R>
where
    R: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AssetResource")
            .field("inner", &self.inner)
            .finish_non_exhaustive()
    }
}

impl<R> Clone for AssetResource<R>
where
    R: Clone,
{
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            _lease: self._lease.clone(),
        }
    }
}

#[async_trait]
impl<R> Resource for AssetResource<R>
where
    R: Resource + Send + Sync,
{
    async fn write(&self, data: &[u8]) -> Result<(), StorageError> {
        self.inner.write(data).await
    }

    async fn read(&self) -> Result<Bytes, StorageError> {
        self.inner.read().await
    }

    async fn commit(&self, final_len: Option<u64>) -> Result<(), StorageError> {
        self.inner.commit(final_len).await
    }

    async fn fail(&self, error: impl Into<String> + Send) -> Result<(), StorageError> {
        self.inner.fail(error).await
    }
}

#[async_trait]
impl StreamingResourceExt for AssetResource<StreamingResource> {
    async fn wait_range(&self, range: Range<u64>) -> Result<WaitOutcome, StorageError> {
        self.inner.wait_range(range).await
    }

    async fn read_at(&self, offset: u64, len: usize) -> Result<Bytes, StorageError> {
        self.inner.read_at(offset, len).await
    }

    async fn write_at(&self, offset: u64, data: &[u8]) -> Result<(), StorageError> {
        self.inner.write_at(offset, data).await
    }
}

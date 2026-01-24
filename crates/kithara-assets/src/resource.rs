#![forbid(unsafe_code)]

use std::{fmt, ops::Range, path::Path, sync::Arc};

use async_trait::async_trait;
use bytes::Bytes;
use kithara_storage::{
    Resource, ResourceStatus, StorageError, StreamingResource, StreamingResourceExt, WaitOutcome,
};

use std::future::Future;
use std::pin::Pin;

/// Callback for recording asset bytes on commit.
pub(crate) type RecordBytesCallback = Arc<dyn Fn(u64) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

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
pub struct AssetResource<R, L = ()> {
    pub(crate) inner: R,
    pub(crate) _lease: L,
    pub(crate) on_commit: Option<RecordBytesCallback>,
}

impl<R, L> AssetResource<R, L> {
    pub(crate) fn new(inner: R, lease: L) -> Self {
        Self {
            inner,
            _lease: lease,
            on_commit: None,
        }
    }

    pub(crate) fn with_commit_callback(inner: R, lease: L, callback: RecordBytesCallback) -> Self {
        Self {
            inner,
            _lease: lease,
            on_commit: Some(callback),
        }
    }

    /// Borrow the inner resource (crate-private).
    ///
    /// This should only be used internally. External code should use trait methods.
    #[allow(dead_code)]
    pub(crate) fn inner(&self) -> &R {
        &self.inner
    }

    /// Consume the wrapper and return the inner resource.
    ///
    /// Note: this also drops the lease guard, releasing the pin.
    pub fn into_inner(self) -> R {
        self.inner
    }
}

/// Специализация для ProcessedResource<StreamingResource> - добавляет метод status().
impl<L, Ctx> AssetResource<crate::ProcessedResource<StreamingResource, Ctx>, L>
where
    Ctx: Clone + std::fmt::Debug,
{
    /// Get resource status.
    pub async fn status(&self) -> ResourceStatus {
        self.inner.status().await
    }
}

impl<R, L> fmt::Debug for AssetResource<R, L>
where
    R: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AssetResource")
            .field("inner", &self.inner)
            .finish_non_exhaustive()
    }
}

impl<R, L> Clone for AssetResource<R, L>
where
    R: Clone,
    L: Clone,
{
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            _lease: self._lease.clone(),
            on_commit: self.on_commit.clone(),
        }
    }
}

#[async_trait]
impl<R, L> Resource for AssetResource<R, L>
where
    R: Resource + Send + Sync,
    L: Send + Sync + 'static,
{
    async fn write(&self, data: &[u8]) -> Result<(), StorageError> {
        self.inner.write(data).await
    }

    async fn read(&self) -> Result<Bytes, StorageError> {
        self.inner.read().await
    }

    async fn commit(&self, final_len: Option<u64>) -> Result<(), StorageError> {
        // Commit the inner resource first
        self.inner.commit(final_len).await?;

        // Record asset bytes if callback is set
        if let Some(ref callback) = self.on_commit {
            tracing::debug!("Commit callback is set, getting file metadata");
            // Get file size from metadata
            if let Ok(metadata) = tokio::fs::metadata(self.inner.path()).await {
                if metadata.is_file() {
                    let size = metadata.len();
                    tracing::debug!(path = ?self.inner.path(), size, "Calling commit callback with file size");
                    callback(size).await;
                    tracing::debug!("Commit callback completed");
                } else {
                    tracing::debug!(path = ?self.inner.path(), "Path is not a file");
                }
            } else {
                tracing::debug!(path = ?self.inner.path(), "Failed to get file metadata");
            }
        } else {
            tracing::debug!("No commit callback set");
        }

        Ok(())
    }

    async fn fail(&self, error: impl Into<String> + Send) -> Result<(), StorageError> {
        self.inner.fail(error).await
    }

    fn path(&self) -> &Path {
        self.inner.path()
    }
}

#[async_trait]
impl<R, L> StreamingResourceExt for AssetResource<R, L>
where
    R: StreamingResourceExt + Send + Sync,
    L: Send + Sync + 'static,
{
    async fn wait_range(&self, range: Range<u64>) -> Result<WaitOutcome, StorageError> {
        self.inner.wait_range(range).await
    }

    async fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, StorageError> {
        self.inner.read_at(offset, buf).await
    }

    async fn write_at(&self, offset: u64, data: &[u8]) -> Result<(), StorageError> {
        self.inner.write_at(offset, data).await
    }
}

//! Cache decorator for downloaders.

use bytes::Bytes;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::trace;

use stream_download::source::{ResourceKey, StreamControl, StreamMsg};
use stream_download::storage::StorageHandle;
use tokio::sync::mpsc;

use super::traits::{ByteStream, Downloader, Headers};
use super::types::Resource;
use crate::error::HlsResult;

/// Where returned bytes came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CacheSource {
    /// Read from cache.
    Cache,
    /// Downloaded from network.
    Network,
}

/// Bytes returned by a cached download.
#[derive(Clone, Debug)]
pub struct CachedBytes {
    /// The downloaded bytes.
    pub bytes: Bytes,
    /// Source of the bytes.
    pub source: CacheSource,
}

/// Callback function type for generating cache keys from resources.
pub type CacheKeyCallback = dyn Fn(&Resource) -> Option<ResourceKey> + Send + Sync;

/// Downloader decorator that adds caching.
pub struct CacheDownloader<D> {
    inner: D,
    handle: StorageHandle,
    /// Callback to generate cache key from resource
    key_callback: Arc<CacheKeyCallback>,
    /// Sender for StoreResource control messages
    data_sender: mpsc::Sender<StreamMsg>,
}

impl<D> CacheDownloader<D>
where
    D: Downloader + Send + Sync,
{
    /// Create a new cache decorator.
    pub fn new(
        inner: D,
        handle: StorageHandle,
        key_callback: Arc<CacheKeyCallback>,
        data_sender: mpsc::Sender<StreamMsg>,
    ) -> Self {
        Self {
            inner,
            handle,
            key_callback,
            data_sender,
        }
    }

    /// Replace the storage handle.
    pub fn with_storage_handle(mut self, handle: StorageHandle) -> Self {
        self.handle = handle;
        self
    }

    /// Set the cache key callback.
    pub fn with_key_callback(mut self, key_callback: Arc<CacheKeyCallback>) -> Self {
        self.key_callback = key_callback;
        self
    }

    /// Set the data sender for StoreResource messages.
    pub fn with_data_sender(mut self, data_sender: mpsc::Sender<StreamMsg>) -> Self {
        self.data_sender = data_sender;
        self
    }

    /// Read from cache.
    fn read_cache(&self, key: &ResourceKey) -> HlsResult<Option<Bytes>> {
        match self.handle.read(key) {
            Ok(Some(bytes)) => Ok(Some(bytes)),
            Err(_) | Ok(None) => Ok(None),
        }
    }

    /// Download with caching using a resource key.
    pub async fn download_cached(
        &self,
        resource: &Resource,
        key: &ResourceKey,
    ) -> HlsResult<CachedBytes> {
        trace!("cache: request url='{}' key='{}'", resource.url(), key.0);
        if let Some(bytes) = self.read_cache(key)? {
            trace!("cache: serving from cache key='{}'", key.0);
            return Ok(CachedBytes {
                bytes,
                source: CacheSource::Cache,
            });
        }

        trace!(
            "cache: downloading from network url='{}' key='{}'",
            resource.url(),
            key.0
        );
        let bytes = self.inner.download_with_headers(resource, None).await?;

        // Send StoreResource message
        let msg = StreamMsg::Control(StreamControl::StoreResource {
            key: key.clone(),
            data: bytes.clone(),
        });
        if let Err(e) = self.data_sender.try_send(msg) {
            trace!(
                "cache: failed to send StoreResource message key='{}' error='{:?}'",
                key.0, e
            );
        } else {
            trace!("cache: sent StoreResource message key='{}'", key.0);
        }

        trace!(
            "cache: downloaded from network url='{}' key='{}' ({} bytes)",
            resource.url(),
            key.0,
            bytes.len()
        );
        Ok(CachedBytes {
            bytes,
            source: CacheSource::Network,
        })
    }

    /// Download with caching using a resource key and custom headers.
    pub async fn download_cached_with_headers(
        &self,
        resource: &Resource,
        key: &ResourceKey,
        headers: Option<Headers>,
    ) -> HlsResult<CachedBytes> {
        trace!(
            "cache: request with headers url='{}' key='{}'",
            resource.url(),
            key.0
        );
        if let Some(bytes) = self.read_cache(key)? {
            trace!("cache: serving from cache key='{}'", key.0);
            return Ok(CachedBytes {
                bytes,
                source: CacheSource::Cache,
            });
        }

        trace!(
            "cache: downloading from network with headers url='{}' key='{}'",
            resource.url(),
            key.0
        );
        let bytes = self.inner.download_with_headers(resource, headers).await?;

        // Send StoreResource message
        let msg = StreamMsg::Control(StreamControl::StoreResource {
            key: key.clone(),
            data: bytes.clone(),
        });
        if let Err(e) = self.data_sender.try_send(msg) {
            trace!(
                "cache: failed to send StoreResource message key='{}' error='{:?}'",
                key.0, e
            );
        } else {
            trace!("cache: sent StoreResource message key='{}'", key.0);
        }

        trace!(
            "cache: downloaded from network url='{}' key='{}' ({} bytes)",
            resource.url(),
            key.0,
            bytes.len()
        );
        Ok(CachedBytes {
            bytes,
            source: CacheSource::Network,
        })
    }
}

#[async_trait::async_trait]
impl<D> Downloader for CacheDownloader<D>
where
    D: Downloader + Send + Sync,
{
    async fn download_with_headers(
        &self,
        resource: &Resource,
        headers: Option<Headers>,
    ) -> HlsResult<Bytes> {
        // Get cache key from callback
        let key = (self.key_callback)(resource);

        if let Some(key) = key {
            // Use caching
            let cached = self
                .download_cached_with_headers(resource, &key, headers)
                .await?;
            Ok(cached.bytes)
        } else {
            // Without a resource key, we can't cache - just pass through
            self.inner.download_with_headers(resource, headers).await
        }
    }

    async fn stream(&self, resource: &Resource) -> HlsResult<ByteStream> {
        // Streaming doesn't use cache
        self.inner.stream(resource).await
    }

    async fn stream_range(
        &self,
        resource: &Resource,
        start: u64,
        end: Option<u64>,
    ) -> HlsResult<ByteStream> {
        // Streaming doesn't use cache
        self.inner.stream_range(resource, start, end).await
    }

    async fn probe_content_length(&self, resource: &Resource) -> HlsResult<Option<u64>> {
        // Probing doesn't use cache
        self.inner.probe_content_length(resource).await
    }

    fn cancel_token(&self) -> &CancellationToken {
        self.inner.cancel_token()
    }
}

impl<D: std::fmt::Debug> std::fmt::Debug for CacheDownloader<D> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CacheDownloader")
            .field("inner", &self.inner)
            .field("handle", &self.handle)
            .field("key_callback", &"Arc<CacheKeyCallback>")
            .field("data_sender", &"mpsc::Sender<StreamMsg>")
            .finish()
    }
}

impl<D: Clone> Clone for CacheDownloader<D> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            handle: self.handle.clone(),
            key_callback: Arc::clone(&self.key_callback),
            data_sender: self.data_sender.clone(),
        }
    }
}

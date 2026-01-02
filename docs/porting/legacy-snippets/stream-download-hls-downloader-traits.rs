//! Downloader traits for HLS resources.

use std::collections::HashMap;

use bytes::Bytes;
use futures_util::stream::BoxStream;

use crate::downloader::types::Resource;
use crate::error::{HlsError, HlsResult};

/// A stream of bytes with potential errors.
pub type ByteStream = BoxStream<'static, Result<Bytes, HlsError>>;

/// Headers for HTTP requests.
pub type Headers = HashMap<String, String>;

/// Core trait for downloading resources.
///
/// This trait provides a unified interface for downloading various types of HLS resources
/// (playlists, keys, segments) with optional headers and range requests.
#[async_trait::async_trait]
pub trait Downloader: Send + Sync {
    /// Download bytes from a resource with custom headers.
    async fn download_with_headers(
        &self,
        resource: &Resource,
        headers: Option<Headers>,
    ) -> HlsResult<Bytes>;

    /// Download bytes from a resource.
    async fn download(&self, resource: &Resource) -> HlsResult<Bytes> {
        self.download_with_headers(resource, None).await
    }

    /// Stream bytes from a resource.
    async fn stream(&self, resource: &Resource) -> HlsResult<ByteStream>;

    /// Stream bytes from a resource with a byte range.
    async fn stream_range(
        &self,
        resource: &Resource,
        start: u64,
        end: Option<u64>,
    ) -> HlsResult<ByteStream>;

    /// Probe content length of a resource.
    async fn probe_content_length(&self, resource: &Resource) -> HlsResult<Option<u64>>;

    /// Get cancellation token for this downloader.
    fn cancel_token(&self) -> &tokio_util::sync::CancellationToken;
}

/// Extension methods for Downloader trait.
#[async_trait::async_trait]
pub trait DownloaderExt: Downloader {
    /// Download a playlist (convenience method).
    async fn download_playlist(&self, resource: &Resource) -> HlsResult<Bytes> {
        self.download(resource).await
    }

    /// Download an encryption key with optional key-specific headers.
    async fn download_key(
        &self,
        resource: &Resource,
        key_headers: Option<Headers>,
    ) -> HlsResult<Bytes> {
        self.download_with_headers(resource, key_headers).await
    }

    /// Download bytes with retry logic (convenience for decorators).
    async fn download_with_retry(&self, resource: &Resource) -> HlsResult<Bytes> {
        self.download(resource).await
    }
}

// Blanket implementation for all Downloader types
#[async_trait::async_trait]
impl<D: Downloader + ?Sized> DownloaderExt for D {}

#![forbid(unsafe_code)]

use std::marker::PhantomData;

use bytes::Bytes;
use kithara_assets::{
    AcquisitionResult, AssetScope, AssetWriter, ReadSide, ResourceKey, WriteSide,
};
use kithara_bufpool::BytePool;
use kithara_net::{Headers, NetError};
use kithara_stream::dl::{FetchCmd, FetchResponse, PeerHandle, reject_html_response};
use tracing::{debug, trace};
use url::Url;

use crate::{HlsError, HlsResult};

/// Marker for a small atomic resource fetched through the disk cache +
/// unified downloader pipeline. The associated `KIND` is used for the
/// asset-store / tracing tag.
pub(crate) trait AtomicResource {
    const KIND: &'static str;
}

#[derive(Clone, Copy)]
pub(crate) struct Playlist;

#[derive(Clone, Copy)]
pub(crate) struct Key;

impl AtomicResource for Playlist {
    const KIND: &'static str = "playlist";
}

impl AtomicResource for Key {
    const KIND: &'static str = "key";
}

/// Typed handle for fetching a small atomic body (playlist or DRM key)
/// through the disk cache + unified downloader pipeline.
pub(crate) struct AtomicFetch<R> {
    downloader: PeerHandle,
    scope: AssetScope,
    byte_pool: BytePool,
    _marker: PhantomData<R>,
}

impl<R> Clone for AtomicFetch<R> {
    fn clone(&self) -> Self {
        Self {
            downloader: self.downloader.clone(),
            scope: self.scope.clone(),
            byte_pool: self.byte_pool.clone(),
            _marker: PhantomData,
        }
    }
}

pub(crate) type PlaylistPeer = AtomicFetch<Playlist>;
pub(crate) type KeyPeer = AtomicFetch<Key>;

impl<R: AtomicResource> AtomicFetch<R> {
    pub(crate) fn new(downloader: PeerHandle, scope: AssetScope, byte_pool: BytePool) -> Self {
        Self {
            downloader,
            scope,
            byte_pool,
            _marker: PhantomData,
        }
    }

    /// Fetch a small atomic body through the disk cache + unified
    /// downloader pipeline.
    ///
    /// Looks the URL up in the cache first; on hit, returns the cached
    /// bytes. On miss, issues a `PeerHandle::execute` with the supplied
    /// headers, then best-effort writes the result back to the cache.
    ///
    /// # Errors
    /// Returns an error when the network fetch fails or when the cache
    /// access layer reports an error other than a harmless concurrent
    /// commit.
    pub(crate) async fn fetch(&self, url: &Url, headers: Option<Headers>) -> HlsResult<Bytes> {
        let key = self.scope.key_from_url(url);
        let rel_path = rel_path_for_log(&key);
        if let Some(bytes) =
            try_read_cached(&self.scope, &self.byte_pool, &key, url, rel_path, R::KIND)?
        {
            return Ok(bytes);
        }

        debug!(
            url = %url,
            asset_root = %self.scope.asset_root(),
            rel_path = %rel_path,
            resource_kind = R::KIND,
            "kithara-hls: cache miss -> fetching from network"
        );

        let writer = match self.scope.store().acquire_resource(&key, None)? {
            AcquisitionResult::Pending(writer) => Some(writer.retain()),
            // Committed by a concurrent caller after our cache-miss probe (or any
            // future variant) — the network bytes below are still correct; skip
            _ => None,
        };
        let bytes = download_atomic_bytes(&self.downloader, url.clone(), headers).await?;

        if let Some(writer) = writer {
            write_back_cache(writer, &bytes, &self.scope, url, rel_path, R::KIND);
        }

        Ok(bytes)
    }

    /// Try to read the resource from the cache. Returns `Ok(Some(bytes))`
    /// on a cache hit, `Ok(None)` on miss.
    ///
    /// # Errors
    /// Returns an error when the cache read fails.
    pub(crate) fn try_cached(&self, url: &Url) -> HlsResult<Option<Bytes>> {
        let key = self.scope.key_from_url(url);
        let rel_path = rel_path_for_log(&key);
        try_read_cached(&self.scope, &self.byte_pool, &key, url, rel_path, R::KIND)
    }

    /// Best-effort cache write of `bytes` under the URL's resource key.
    /// Acquires the writer (commit ordering matches the network path:
    /// acquire after the bytes are in hand). On a concurrent commit the
    /// resource is already `Committed` and the write is skipped.
    ///
    /// # Errors
    /// Returns an error when acquiring the resource fails.
    pub(crate) fn write_back(&self, url: &Url, bytes: &Bytes) -> HlsResult<()> {
        let key = self.scope.key_from_url(url);
        let rel_path = rel_path_for_log(&key);
        if let AcquisitionResult::Pending(writer) =
            self.scope.store().acquire_resource(&key, None)?
        {
            write_back_cache(writer.retain(), bytes, &self.scope, url, rel_path, R::KIND);
        }
        Ok(())
    }

    /// Execute a custom fetch command through the underlying downloader.
    ///
    /// # Errors
    /// Returns the underlying [`NetError`] when the fetch fails.
    pub(crate) async fn execute(&self, cmd: FetchCmd) -> Result<FetchResponse, NetError> {
        self.downloader.execute(cmd).await
    }
}

/// Best-effort cache write. Concurrent callers may race: all miss
/// the cache, all fetch, first commits, later writes fail because the
/// resource is already committed. Harmless — the bytes are in memory
/// from the network fetch. Consumes the writer (commit is consume-self).
fn write_back_cache(
    writer: AssetWriter,
    bytes: &Bytes,
    scope: &AssetScope,
    url: &Url,
    rel_path: &str,
    resource_kind: &str,
) {
    let final_len = match u64::try_from(bytes.len()) {
        Ok(len) => len,
        Err(e) => {
            trace!(
                url = %url,
                error = %e,
                resource_kind,
                "kithara-hls: cache write skipped because body length cannot fit u64"
            );
            return;
        }
    };
    let result = writer
        .write_at(0, bytes)
        .and_then(|()| writer.commit(Some(final_len)).map(|_reader| ()));
    if let Err(e) = result {
        trace!(
            url = %url,
            error = %e,
            resource_kind,
            "kithara-hls: cache write failed (concurrent commit), using network bytes"
        );
    } else {
        debug!(
            url = %url,
            asset_root = %scope.asset_root(),
            rel_path = %rel_path,
            bytes = bytes.len(),
            resource_kind,
            "kithara-hls: fetched from network and cached"
        );
    }
}

fn rel_path_for_log(key: &ResourceKey) -> &str {
    key.rel_path().unwrap_or("<absolute>")
}

fn try_read_cached(
    scope: &AssetScope,
    byte_pool: &BytePool,
    key: &ResourceKey,
    url: &Url,
    rel_path: &str,
    resource_kind: &str,
) -> HlsResult<Option<Bytes>> {
    let Ok(res) = scope.store().open_resource(key, None) else {
        return Ok(None);
    };
    let mut buf = byte_pool.get();
    let n = res.read_into(&mut buf)?;
    if n == 0 {
        return Ok(None);
    }
    let _pinned = res.retain();
    debug!(
        url = %url,
        asset_root = %scope.asset_root(),
        rel_path = %rel_path,
        bytes = n,
        resource_kind,
        "kithara-hls: cache hit (pinned)"
    );
    Ok(Some(Bytes::copy_from_slice(&buf)))
}

/// Fetch a URL and collect the full body into a `Bytes` buffer.
async fn download_atomic_bytes(
    downloader: &PeerHandle,
    url: Url,
    headers: Option<Headers>,
) -> HlsResult<Bytes> {
    let cmd = FetchCmd::get(url)
        .maybe_headers(headers)
        .validator(reject_html_response)
        .build();
    let resp = downloader.execute(cmd).await.map_err(HlsError::from)?;
    resp.body.collect().await.map_err(HlsError::from)
}

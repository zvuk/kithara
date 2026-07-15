#![forbid(unsafe_code)]

use std::{io::ErrorKind, marker::PhantomData};

use bytes::Bytes;
use kithara_assets::{
    AcquisitionResult, AssetScope, AssetWriter, AssetsError, ReadSide, ResourceKey, WriteSide,
};
use kithara_bufpool::BytePool;
use kithara_net::{Headers, NetError};
use kithara_stream::dl::{FetchCmd, FetchResponse, PeerHandle, reject_html_response};
use tracing::{debug, warn};
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
    scope: AssetScope,
    byte_pool: BytePool,
    downloader: PeerHandle,
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

    /// Execute a custom fetch command through the underlying downloader.
    ///
    /// # Errors
    /// Returns the underlying [`NetError`] when the fetch fails.
    pub(crate) async fn execute(&self, cmd: FetchCmd) -> Result<FetchResponse, NetError> {
        self.downloader.execute(cmd).await
    }

    async fn download(
        &self,
        key: &ResourceKey,
        url: &Url,
        headers: Option<Headers>,
    ) -> HlsResult<Bytes> {
        debug!(
            url = %url,
            asset_root = %self.scope.asset_root(),
            rel_path = %rel_path_for_log(key),
            resource_kind = R::KIND,
            "kithara-hls: fetching from network"
        );
        download_atomic_bytes(&self.downloader, url.clone(), headers).await
    }

    fn invalidate(&self, key: &ResourceKey) -> HlsResult<()> {
        self.scope.store().remove_resource(key)?;
        Ok(())
    }

    /// Try to read the resource from the cache. Returns `Ok(Some(bytes))`
    /// on a cache hit, `Ok(None)` on miss.
    ///
    /// # Errors
    /// Returns an error when the cache read fails.
    pub(crate) fn try_cached(&self, key: &ResourceKey, url: &Url) -> HlsResult<Option<Bytes>> {
        let rel_path = rel_path_for_log(key);
        try_read_cached(&self.scope, &self.byte_pool, key, url, rel_path, R::KIND)
    }

    /// Cache `bytes` under the URL's resource key.
    /// Acquires the writer after the bytes are in hand. An already committed
    /// resource is left unchanged.
    ///
    /// # Errors
    /// Returns an error when acquiring the resource fails.
    pub(crate) fn write_back(&self, key: &ResourceKey, url: &Url, bytes: &Bytes) -> HlsResult<()> {
        let rel_path = rel_path_for_log(key);
        if let AcquisitionResult::Pending(writer) =
            self.scope.store().acquire_resource(key, None)?
        {
            write_back_cache(writer.retain(), bytes, &self.scope, url, rel_path, R::KIND);
        }
        Ok(())
    }
}

impl<R: AtomicResource> AtomicFetch<R> {
    /// Fetch and validate an atomic resource within one store transaction.
    /// # Errors
    /// Returns an error when cache invalidation, network fetch, validation, or
    /// cache acquisition fails.
    pub(crate) async fn fetch_validated<T, F>(
        &self,
        key: &ResourceKey,
        url: &Url,
        headers: Option<Headers>,
        validate: F,
    ) -> HlsResult<T>
    where
        F: Fn(&[u8]) -> HlsResult<T>,
    {
        let store = self.scope.store().clone();
        store
            .with_resource_transaction(key, || async {
                self.fetch_validated_inner(key, url, headers, validate)
                    .await
            })
            .await
    }

    async fn fetch_validated_inner<T, F>(
        &self,
        key: &ResourceKey,
        url: &Url,
        headers: Option<Headers>,
        validate: F,
    ) -> HlsResult<T>
    where
        F: Fn(&[u8]) -> HlsResult<T>,
    {
        if let Some(bytes) = self.try_cached(key, url)? {
            match validate(&bytes) {
                Ok(value) => return Ok(value),
                Err(error) => {
                    warn!(
                        url = %url,
                        error = %error,
                        resource_kind = R::KIND,
                        "kithara-hls: cached atomic resource is invalid; removing it before refetch"
                    );
                    self.invalidate(key)?;
                }
            }
        }

        let bytes = self.download(key, url, headers).await?;
        let value = validate(&bytes)?;
        if let Err(error) = self.write_back(key, url, &bytes) {
            warn!(
                url = %url,
                error = %error,
                resource_kind = R::KIND,
                "kithara-hls: valid atomic resource could not be persisted; using network bytes"
            );
        }
        Ok(value)
    }
}

fn write_back_cache(
    writer: AssetWriter,
    bytes: &Bytes,
    scope: &AssetScope,
    url: &Url,
    rel_path: &str,
    resource_kind: &str,
) -> bool {
    let Ok(final_len) = u64::try_from(bytes.len()) else {
        warn!(
            url = %url,
            resource_kind,
            "kithara-hls: cache write skipped because body length does not fit u64"
        );
        return false;
    };
    let result = writer
        .write_at(0, bytes)
        .and_then(|()| writer.commit(Some(final_len)).map(|_reader| ()));
    if let Err(error) = result {
        warn!(
            url = %url,
            error = %error,
            resource_kind,
            "kithara-hls: cache write failed"
        );
        return false;
    }
    debug!(
        url = %url,
        asset_root = %scope.asset_root(),
        rel_path = %rel_path,
        bytes = bytes.len(),
        resource_kind,
        "kithara-hls: fetched from network and cached"
    );
    true
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
    let res = match scope.store().open_resource(key, None) {
        Ok(resource) => resource,
        Err(AssetsError::Io(error)) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let mut buf = byte_pool.get();
    let n = res.read_into(&mut buf)?;
    if n == 0 {
        warn!(
            url = %url,
            asset_root = %scope.asset_root(),
            rel_path,
            resource_kind,
            "kithara-hls: cached atomic resource is empty; removing it before refetch"
        );
        scope.store().remove_resource(key)?;
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

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use kithara_assets::{AssetStoreBuilder, StorageBackend};
    use kithara_platform::CancelToken;
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn cache_write_failure_is_nonfatal() {
        let cancel = CancelToken::never();
        let store = AssetStoreBuilder::default()
            .backend(StorageBackend::Memory)
            .cancel(cancel.clone())
            .build();
        let scope = store.scope("failed-cache-write");
        let url = Url::parse("https://example.com/master.m3u8").unwrap();
        let key = scope.key_for(&url);
        let AcquisitionResult::Pending(writer) =
            scope.store().acquire_resource(&key, None).unwrap()
        else {
            panic!("fresh resource must be pending");
        };
        cancel.cancel();

        assert!(!write_back_cache(
            writer,
            &Bytes::from_static(b"#EXTM3U\n"),
            &scope,
            &url,
            rel_path_for_log(&key),
            Playlist::KIND,
        ));
    }
}

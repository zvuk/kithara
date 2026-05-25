#![forbid(unsafe_code)]

use bytes::Bytes;
use kithara_assets::{AssetScope, ResourceKey};
use kithara_bufpool::BytePool;
use kithara_drm::DecryptContext;
use kithara_net::Headers;
use kithara_storage::ResourceExt;
use kithara_stream::dl::{FetchCmd, PeerHandle, reject_html_response};
use tracing::{debug, trace};
use url::Url;

use crate::{HlsError, HlsResult};

/// Fetch a small atomic body (playlist or DRM key) through the disk
/// cache + unified downloader pipeline.
///
/// Looks the URL up in `backend` first; on hit, returns the cached
/// bytes. On miss, issues a `PeerHandle::execute` with the supplied
/// headers, then best-effort writes the result back to the cache.
///
/// # Errors
/// Returns an error when the network fetch fails or when the cache
/// access layer reports an error other than a harmless concurrent
/// commit.
pub(crate) async fn fetch_atomic_body(
    downloader: &PeerHandle,
    scope: &AssetScope<DecryptContext>,
    byte_pool: &BytePool,
    headers: Option<Headers>,
    url: &Url,
    rel_path: &str,
    resource_kind: &str,
) -> HlsResult<Bytes> {
    let key = scope.key(rel_path);
    if let Some(bytes) = try_read_cached(scope, byte_pool, &key, url, rel_path, resource_kind)? {
        return Ok(bytes);
    }

    debug!(
        url = %url,
        asset_root = %scope.asset_root(),
        rel_path = %rel_path,
        resource_kind,
        "kithara-hls: cache miss -> fetching from network"
    );

    let res = scope.store().acquire_resource(&key, None)?.retain();
    let bytes = download_atomic_bytes(downloader, url.clone(), headers).await?;

    write_back_cache(&res, &bytes, scope, url, rel_path, resource_kind);

    Ok(bytes)
}

/// Try to read the resource from the cache. Returns `Ok(Some(bytes))`
/// on a cache hit, `Ok(None)` on miss.
pub(crate) fn try_read_cached(
    scope: &AssetScope<DecryptContext>,
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

/// Best-effort cache write. Concurrent callers may race: all miss
/// the cache, all fetch, first commits, later `write_all` calls
/// fail because the resource is already committed. Harmless — the
/// bytes are in memory from the network fetch.
pub(crate) fn write_back_cache(
    res: &kithara_assets::AssetResource<DecryptContext>,
    bytes: &Bytes,
    scope: &AssetScope<DecryptContext>,
    url: &Url,
    rel_path: &str,
    resource_kind: &str,
) {
    if let Err(e) = res.write_all(bytes) {
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

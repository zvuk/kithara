#![forbid(unsafe_code)]

//! Shared helper for fetching small atomic bodies (playlists, DRM keys)
//! through the disk cache + unified downloader pipeline.
//!
//! Both [`crate::playlist_cache::PlaylistCache`] and [`crate::keys::KeyManager`]
//! call into this helper so the resource lookup, network fallback, and
//! cache write-back logic is not duplicated.

use bytes::Bytes;
use kithara_assets::{AssetStore, ResourceKey};
use kithara_bufpool::byte_pool;
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
    backend: &AssetStore<DecryptContext>,
    headers: Option<Headers>,
    url: &Url,
    rel_path: &str,
    resource_kind: &str,
) -> HlsResult<Bytes> {
    let key = ResourceKey::from_url(url);
    if let Ok(res) = backend.open_resource(&key) {
        let mut buf = byte_pool().get();
        let n = res.read_into(&mut buf)?;
        if n > 0 {
            let _pinned = res.retain();
            debug!(
                url = %url,
                asset_root = %backend.asset_root(),
                rel_path = %rel_path,
                bytes = n,
                resource_kind,
                "kithara-hls: cache hit (pinned)"
            );
            return Ok(Bytes::copy_from_slice(&buf));
        }
    }

    debug!(
        url = %url,
        asset_root = %backend.asset_root(),
        rel_path = %rel_path,
        resource_kind,
        "kithara-hls: cache miss -> fetching from network"
    );

    let res = backend.acquire_resource(&key)?.retain();
    let bytes = download_atomic_bytes(downloader, url.clone(), headers).await?;

    // Best-effort cache write. Concurrent callers may race: all miss
    // the cache, all fetch, first commits, later `write_all` calls
    // fail because the resource is already committed. Harmless — the
    // bytes are in memory from the network fetch.
    if let Err(e) = res.write_all(&bytes) {
        trace!(
            url = %url,
            error = %e,
            resource_kind,
            "kithara-hls: cache write failed (concurrent commit), using network bytes"
        );
    } else {
        debug!(
            url = %url,
            asset_root = %backend.asset_root(),
            rel_path = %rel_path,
            bytes = bytes.len(),
            resource_kind,
            "kithara-hls: fetched from network and cached"
        );
    }

    Ok(bytes)
}

/// Fetch a URL and collect the full body into a `Bytes` buffer.
async fn download_atomic_bytes(
    downloader: &PeerHandle,
    url: Url,
    headers: Option<Headers>,
) -> HlsResult<Bytes> {
    let cmd = FetchCmd::get(url)
        .headers(headers)
        .with_validator(reject_html_response);
    let resp = downloader.execute(cmd).await.map_err(HlsError::from)?;
    resp.body.collect().await.map_err(HlsError::from)
}

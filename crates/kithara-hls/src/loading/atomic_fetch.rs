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
use kithara_stream::dl::{FetchCmd, FetchMethod, FetchResult as DlFetchResult, TrackHandle};
use tracing::{debug, trace};
use url::Url;

use crate::{HlsError, HlsResult};

/// Fetch a small atomic body (playlist or DRM key) through the disk
/// cache + unified downloader pipeline.
///
/// Looks the URL up in `backend` first; on hit, returns the cached
/// bytes. On miss, issues a `Downloader::execute(FetchCmd::Get)` with
/// the supplied headers, then best-effort writes the result back to
/// the cache. Concurrent writers may race — whoever commits the
/// resource first wins, and subsequent writes silently fail (the
/// bytes are still returned to the caller from the network fetch).
///
/// # Errors
/// Returns an error when the network fetch fails or when the cache
/// access layer reports an error other than a harmless concurrent
/// commit.
pub(crate) async fn fetch_atomic_body(
    downloader: &TrackHandle,
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
            debug!(
                url = %url,
                asset_root = %backend.asset_root(),
                rel_path = %rel_path,
                bytes = n,
                resource_kind,
                "kithara-hls: cache hit"
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

    let res = backend.acquire_resource(&key)?;
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

/// Drive a single `Downloader::execute` call that accumulates the full
/// body into a `Bytes` buffer.
async fn download_atomic_bytes(
    downloader: &TrackHandle,
    url: Url,
    headers: Option<Headers>,
) -> HlsResult<Bytes> {
    let cmd = FetchCmd {
        method: FetchMethod::Get,
        url,
        range: None,
        headers,
        on_connect: None,
        writer: None,
        on_complete: None,
        throttle: None,
    };
    match downloader.execute(cmd).await {
        DlFetchResult::Ok { body, .. } => Ok(body.map(Bytes::from).unwrap_or_default()),
        DlFetchResult::Err(e) => Err(HlsError::from(e)),
    }
}

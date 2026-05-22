#![forbid(unsafe_code)]

use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
};

use bytes::Bytes;
use kithara_assets::{AssetStore, ResourceKey};
use kithara_drm::{DecryptContext, KeyProcessorRegistry};
use kithara_net::Headers;
use kithara_storage::ResourceExt;
use kithara_stream::dl::{FetchCmd, PeerHandle};
use url::Url;

use super::atomic_fetch::fetch_atomic_body;
use crate::{HlsError, HlsResult};

/// DRM key fetch + processor pipeline.
///
/// Routes per-provider processing (headers, query params, key
/// decryption) through a [`KeyProcessorRegistry`] — each key URL's
/// host is looked up in the registry to pick the matching rule.
#[derive(Clone)]
pub struct KeyManager {
    /// In-memory hot-path cache of decrypted keys for rule-matched URLs.
    ///
    /// `get_cached_key` reads from here under a synchronous segment
    /// fetch, so it must be zero-I/O. The plaintext key is also
    /// persisted to the [`AssetStore`] by [`Self::get_raw_key`] under
    /// the same `ResourceKey::from_url(key_url)` as plain HLS-AES keys
    /// — re-opening the same track in a later session resolves through
    /// disk cache without re-hitting the key endpoint. The cached
    /// **plaintext** is deterministic per track/quality and safe to
    /// persist; the on-the-wire response is encrypted with a fresh
    /// per-session seed and never touches disk.
    decrypted_keys: Arc<Mutex<HashMap<Url, Bytes>>>,
    backend: AssetStore<DecryptContext>,
    /// Byte buffer pool for reading cached key bodies.
    byte_pool: kithara_bufpool::BytePool,
    /// Cache-wide headers (typically equal to `HlsConfig::headers`).
    base_headers: Option<Headers>,
    key_registry: Option<KeyProcessorRegistry>,
    downloader: PeerHandle,
}

impl KeyManager {
    /// AES-128 key / IV length in bytes.
    const AES_KEY_LEN: usize = 16;

    /// Start offset for sequence number in the 16-byte IV.
    const IV_SEQUENCE_OFFSET: usize = 8;

    /// Asset-store / tracing tag for HLS-AES key resources — used for
    /// plain and DRM (registry-matched, decrypted-on-read) keys alike.
    /// They share a single `ResourceKey::from_url(key_url)` slot in
    /// the asset store; emitting the same kind everywhere keeps
    /// cache-hit / cache-miss telemetry grouped under one label.
    pub(crate) const RESOURCE_KIND: &str = "key";

    #[must_use]
    pub fn new(
        downloader: PeerHandle,
        backend: AssetStore<DecryptContext>,
        base_headers: Option<Headers>,
        key_registry: Option<KeyProcessorRegistry>,
        byte_pool: kithara_bufpool::BytePool,
    ) -> Self {
        Self {
            downloader,
            backend,
            base_headers,
            key_registry,
            byte_pool,
            decrypted_keys: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn aes128_key_urls(media_playlists: &[crate::parsing::MediaPlaylist]) -> HashSet<Url> {
        let mut urls: HashSet<Url> = HashSet::new();
        for playlist in media_playlists {
            for segment in &playlist.segments {
                if let Some(ref seg_key) = segment.key
                    && matches!(seg_key.method, crate::parsing::EncryptionMethod::Aes128)
                    && let Some(ref key_info) = seg_key.key_info
                    && let Ok(seg_url) = playlist.url.join(&segment.uri)
                    && let Ok(key_url) = Self::resolve_key_url(key_info, &seg_url)
                {
                    urls.insert(key_url);
                }
            }
        }
        urls
    }

    pub(crate) fn derive_iv(
        key_info: &crate::parsing::KeyInfo,
        sequence: u64,
    ) -> [u8; Self::AES_KEY_LEN] {
        if let Some(iv) = key_info.iv {
            return iv;
        }
        let mut iv = [0u8; Self::AES_KEY_LEN];
        iv[Self::IV_SEQUENCE_OFFSET..].copy_from_slice(&sequence.to_be_bytes());
        iv
    }

    /// Convenience constructor from [`crate::config::KeyOptions`].
    #[must_use]
    pub fn from_options(
        downloader: PeerHandle,
        backend: AssetStore<DecryptContext>,
        base_headers: Option<Headers>,
        options: crate::config::KeyOptions,
        byte_pool: kithara_bufpool::BytePool,
    ) -> Self {
        Self::new(
            downloader,
            backend,
            base_headers,
            options.key_registry,
            byte_pool,
        )
    }

    /// Synchronous key lookup — no I/O.
    ///
    /// DRM keys come from the in-memory map populated by a prior
    /// [`Self::get_raw_key`] call on this session. Non-DRM keys come
    /// from the persistent [`AssetStore`] disk cache.
    ///
    /// # Errors
    /// Returns an error when the key hasn't been fetched yet.
    pub fn get_cached_key(&self, url: &Url) -> HlsResult<Bytes> {
        let rule = self.key_registry.as_ref().and_then(|r| r.find(url));
        if rule.is_some() {
            return self
                .decrypted_keys
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(url)
                .cloned()
                .ok_or_else(|| HlsError::KeyProcessing(format!("DRM key not prefetched: {url}")));
        }

        let cache_key = ResourceKey::from_url(url);
        let res = self
            .backend
            .open_resource(&cache_key)
            .map_err(|e| HlsError::KeyProcessing(format!("key not in cache: {url} — {e}")))?;
        let mut buf = self.byte_pool.get();
        let n = res.read_into(&mut buf).map_err(|e| {
            HlsError::KeyProcessing(format!("failed to read cached key: {url} — {e}"))
        })?;
        if n == 0 {
            return Err(HlsError::KeyProcessing(format!(
                "cached key is empty: {url}",
            )));
        }
        Ok(Bytes::copy_from_slice(&buf[..n]))
    }

    /// Load, optionally preprocess, and return the final key bytes.
    ///
    /// Looks up the matching rule in [`KeyProcessorRegistry`] by the
    /// key URL's host, applies the rule's query params + headers to
    /// the request, and runs the rule's processor on the response.
    ///
    /// Rule-matched (DRM) and plain AES-128 keys both persist to the
    /// [`AssetStore`] under `ResourceKey::from_url(key_url)`. DRM keys
    /// get decrypted via `rule.processor()` before the write-back so
    /// the cached bytes are the **plaintext** AES-128 key — the
    /// per-session seed on the wire never reaches disk. Non-DRM keys
    /// share the same path through [`fetch_atomic_body`].
    ///
    /// # Errors
    /// Returns an error when the fetch or the processor fails.
    ///
    /// # Panics
    /// Panics only if the `rule.is_none()` branch fallthrough is reached
    /// despite matching the `is_none()` guard — this is logically
    /// impossible, expressed via `expect` instead of an unreachable
    /// assertion so the reason is attributed at the call site.
    pub async fn get_raw_key(
        &self,
        url: &Url,
        _iv: Option<[u8; Self::AES_KEY_LEN]>,
    ) -> HlsResult<Bytes> {
        let rule = self.key_registry.as_ref().and_then(|r| r.find(url));
        if rule.is_none() {
            let headers = self.merged_headers(None);
            let rel_path = rel_path_from_url(url);
            return fetch_atomic_body(
                &self.downloader,
                &self.backend,
                &self.byte_pool,
                headers,
                url,
                rel_path.as_str(),
                Self::RESOURCE_KIND,
            )
            .await;
        }

        if let Some(cached) = self
            .decrypted_keys
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(url)
        {
            tracing::debug!(%url, "drm key: served from in-memory cache");
            return Ok(cached.clone());
        }

        let cache_key = ResourceKey::from_url(url);
        let rel_path = rel_path_from_url(url);
        if let Some(bytes) = super::atomic_fetch::try_read_cached(
            &self.backend,
            &self.byte_pool,
            &cache_key,
            url,
            rel_path.as_str(),
            Self::RESOURCE_KIND,
        )? {
            tracing::info!(%url, bytes = bytes.len(), "drm key: served from disk cache");
            self.decrypted_keys
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(url.clone(), bytes.clone());
            return Ok(bytes);
        }

        let rule = rule.expect("rule matched above");
        let mut fetch_url = url.clone();
        if let Some(params) = rule.query_params.as_ref() {
            let mut pairs = fetch_url.query_pairs_mut();
            for (k, v) in params {
                pairs.append_pair(k, v);
            }
        }
        let request = rule.build_request();
        let mut combined_headers: HashMap<String, String> =
            rule.headers.clone().unwrap_or_default();
        combined_headers.extend(request.headers);
        let headers = self.merged_headers(Some(&combined_headers));

        tracing::info!(%url, "drm key: fetching from network");
        let cmd = FetchCmd::get(fetch_url).maybe_headers(headers).build();
        let resp = self.downloader.execute(cmd).await.map_err(|e| {
            tracing::warn!(%url, error = %e, "drm key: network fetch failed");
            HlsError::from(e)
        })?;
        let raw_key = resp.body.collect().await.map_err(|e| {
            tracing::warn!(%url, error = %e, "drm key: body collect failed");
            HlsError::from(e)
        })?;

        let decrypted = (request.processor)(raw_key).map_err(|e| {
            tracing::warn!(%url, error = %e, "drm key: registry processor (decrypt) failed");
            HlsError::KeyProcessing(format!("registry processor: {e}"))
        })?;
        tracing::info!(
            %url,
            bytes = decrypted.len(),
            "drm key: fetched + decrypted, caching to asset store"
        );

        let res = self.backend.acquire_resource(&cache_key)?.retain();
        super::atomic_fetch::write_back_cache(
            &res,
            &decrypted,
            &self.backend,
            url,
            rel_path.as_str(),
            Self::RESOURCE_KIND,
        );

        self.decrypted_keys
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(url.clone(), decrypted.clone());
        Ok(decrypted)
    }

    /// Merge per-rule headers on top of base headers.
    /// Rule-specific entries take precedence on key conflict.
    fn merged_headers(&self, rule_headers: Option<&HashMap<String, String>>) -> Option<Headers> {
        let extra = rule_headers.cloned().map(Headers::from);
        match (self.base_headers.clone(), extra) {
            (None, None) => None,
            (Some(base), None) => Some(base),
            (None, Some(req)) => Some(req),
            (Some(base), Some(req)) => {
                let mut merged = base;
                for (k, v) in req.iter() {
                    merged.insert(k, v);
                }
                Some(merged)
            }
        }
    }

    /// Eagerly fetch every AES-128 key referenced by `media_playlists`,
    /// so [`Self::resolve_decrypt_ctx`] can serve them synchronously
    /// during variant construction. Cleartext playlists yield no
    /// fetches and the call resolves immediately.
    ///
    /// # Errors
    /// Returns the first [`HlsError`] from an underlying key fetch.
    pub(crate) async fn prefetch_aes128_keys(
        &self,
        media_playlists: &[crate::parsing::MediaPlaylist],
    ) -> HlsResult<()> {
        let urls = Self::aes128_key_urls(media_playlists);
        let futs = urls
            .into_iter()
            .map(|url| async move { self.get_raw_key(&url, None).await.map(drop) });
        futures::future::try_join_all(futs).await?;
        Ok(())
    }

    /// Build a [`DecryptContext`] from a pre-fetched key. Glues
    /// `resolve_key_url` + `get_cached_key` + `derive_iv` into a single
    /// sync step that runs after [`Self::get_raw_key`] has populated the
    /// cache. Caller invokes this only for AES-128 segments — the
    /// presence of [`KeyInfo`](crate::parsing::KeyInfo) means a key is
    /// required, so every failure path here is a real error.
    ///
    /// # Errors
    /// - [`HlsError::InvalidUrl`] when the key URI cannot be resolved.
    /// - [`HlsError::KeyProcessing`] when the cached key is missing,
    ///   empty, or not 16 bytes (AES-128 size).
    pub(crate) fn resolve_decrypt_ctx(
        &self,
        key_info: &crate::parsing::KeyInfo,
        segment_url: &Url,
        sequence: u64,
    ) -> HlsResult<DecryptContext> {
        let key_url = Self::resolve_key_url(key_info, segment_url)?;
        let key_bytes = self.get_cached_key(&key_url)?;
        let key: [u8; Self::AES_KEY_LEN] = key_bytes.as_ref().try_into().map_err(|_| {
            HlsError::KeyProcessing(format!(
                "AES-128 key must be {} bytes, got {} for {key_url}",
                Self::AES_KEY_LEN,
                key_bytes.len()
            ))
        })?;
        Ok(DecryptContext::new(
            key,
            Self::derive_iv(key_info, sequence),
        ))
    }

    /// Resolve the [`DecryptContext`] for a variant's init segment.
    /// Returns `None` when the playlist has no init segment, when the
    /// init segment carries no key (cleartext), or when key resolution
    /// fails (same fallback policy as media segments).
    pub(crate) fn resolve_init_decrypt_ctx(
        &self,
        playlist: &crate::parsing::MediaPlaylist,
    ) -> Option<DecryptContext> {
        let init = playlist.init_segment.as_ref()?;
        let key = init.key.as_ref()?;
        if !matches!(key.method, crate::parsing::EncryptionMethod::Aes128) {
            return None;
        }
        let key_info = key.key_info.as_ref()?;
        let init_url = playlist.url.join(&init.uri).ok()?;
        match self.resolve_decrypt_ctx(key_info, &init_url, 0) {
            Ok(ctx) => Some(ctx),
            Err(e) => {
                tracing::warn!(
                    url = %init_url,
                    error = %e,
                    "resolve_init_decrypt_ctx failed; init will be unreadable",
                );
                None
            }
        }
    }

    pub(crate) fn resolve_key_url(
        key_info: &crate::parsing::KeyInfo,
        segment_url: &Url,
    ) -> HlsResult<Url> {
        let key_uri = key_info
            .uri
            .as_ref()
            .ok_or_else(|| HlsError::InvalidUrl("missing key URI".to_string()))?;

        if key_uri.starts_with("http://") || key_uri.starts_with("https://") {
            Url::parse(key_uri).map_err(|e| HlsError::InvalidUrl(format!("Invalid key URL: {e}")))
        } else {
            segment_url
                .join(key_uri)
                .map_err(|e| HlsError::InvalidUrl(format!("Failed to resolve key URL: {e}")))
        }
    }

    fn resolve_segment_decrypt_ctx(
        &self,
        media_url: &Url,
        segment: &crate::parsing::MediaSegment,
    ) -> Option<DecryptContext> {
        let key = segment.key.as_ref()?;
        if !matches!(key.method, crate::parsing::EncryptionMethod::Aes128) {
            return None;
        }
        let key_info = key.key_info.as_ref()?;
        let segment_url = media_url.join(&segment.uri).ok()?;
        match self.resolve_decrypt_ctx(key_info, &segment_url, segment.sequence) {
            Ok(ctx) => Some(ctx),
            Err(e) => {
                tracing::warn!(
                    url = %segment_url,
                    sequence = segment.sequence,
                    error = %e,
                    "resolve_decrypt_ctx failed; segment will be unreadable",
                );
                None
            }
        }
    }

    /// Resolve one [`DecryptContext`] per segment of `playlist`, using
    /// keys already pre-fetched by [`Self::prefetch_aes128_keys`].
    /// Cleartext segments map to `None`; AES-128 segments map to
    /// `Some(ctx)`. A resolution failure on an encrypted segment is
    /// logged and the segment falls back to `None` — the dispatch path
    /// will surface a decoder error, which is preferable to silently
    /// shipping garbage bytes.
    pub(crate) fn resolve_variant_decrypt_contexts(
        &self,
        playlist: &crate::parsing::MediaPlaylist,
    ) -> Vec<Option<DecryptContext>> {
        playlist
            .segments
            .iter()
            .map(|segment| self.resolve_segment_decrypt_ctx(&playlist.url, segment))
            .collect()
    }
}

/// Derive a safe disk cache basename from the key URL.
fn rel_path_from_url(url: &Url) -> String {
    let last = url
        .path_segments()
        .and_then(|mut segments| segments.next_back())
        .unwrap_or("index");
    if let Some((stem, _)) = last.rsplit_once('.')
        && !stem.is_empty()
    {
        return stem.to_string();
    }
    last.to_string()
}

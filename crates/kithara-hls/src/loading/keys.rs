#![forbid(unsafe_code)]

//! DRM key fetch + processor pipeline.
//!
//! Owns the disk cache + downloader handles directly — no dependency
//! on `FetchManager`. Shares the same atomic-body helper as
//! [`crate::playlist_cache::PlaylistCache`] so the cache lookup,
//! network fetch, and write-back logic is not duplicated.

use std::{
    collections::HashMap,
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
    downloader: PeerHandle,
    backend: AssetStore<DecryptContext>,
    /// Cache-wide headers (typically equal to `HlsConfig::headers`).
    base_headers: Option<Headers>,
    key_registry: Option<KeyProcessorRegistry>,
    /// In-memory store of decrypted keys for rule-matched URLs.
    ///
    /// Rule-matched (DRM) key responses are seed-dependent: the server
    /// returns `cipher.encrypt(real_key, session_seed)` where `seed` is
    /// a fresh random value per `KeyManager` instance. Caching the
    /// encrypted response on disk would poison the next session (new
    /// seed can't decrypt the old response). The plaintext key is
    /// deterministic per track/quality, so we cache it in-memory for
    /// the lifetime of the stream — `get_cached_key` stays zero-I/O
    /// on the segment hot path.
    decrypted_keys: Arc<Mutex<HashMap<Url, Bytes>>>,
}

impl KeyManager {
    /// AES-128 key / IV length in bytes.
    const AES_KEY_LEN: usize = 16;

    /// Start offset for sequence number in the 16-byte IV.
    const IV_SEQUENCE_OFFSET: usize = 8;

    #[must_use]
    pub fn new(
        downloader: PeerHandle,
        backend: AssetStore<DecryptContext>,
        base_headers: Option<Headers>,
        key_registry: Option<KeyProcessorRegistry>,
    ) -> Self {
        Self {
            downloader,
            backend,
            base_headers,
            key_registry,
            decrypted_keys: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Convenience constructor from [`crate::config::KeyOptions`].
    #[must_use]
    pub fn from_options(
        downloader: PeerHandle,
        backend: AssetStore<DecryptContext>,
        base_headers: Option<Headers>,
        options: crate::config::KeyOptions,
    ) -> Self {
        Self::new(downloader, backend, base_headers, options.key_registry)
    }

    /// Load, optionally preprocess, and return the final key bytes.
    ///
    /// Looks up the matching rule in [`KeyProcessorRegistry`] by the
    /// key URL's host, applies the rule's query params + headers to
    /// the request, and runs the rule's processor on the response.
    ///
    /// Rule-matched (DRM) keys bypass the on-disk cache entirely and
    /// live only in an in-process map keyed by the original URL. The
    /// server response is encrypted with a session-local random seed,
    /// so caching it on disk would poison the next session. Non-DRM
    /// keys (plain AES-128 `key` files) go through `fetch_atomic_body`
    /// like other small bodies.
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
            // Plain (non-DRM) key: disk-cache the raw bytes as before.
            let headers = self.merged_headers(None);
            let rel_path = rel_path_from_url(url);
            return fetch_atomic_body(
                &self.downloader,
                &self.backend,
                headers,
                url,
                rel_path.as_str(),
                "key",
            )
            .await;
        }

        // Memoized DRM path: serve decrypted bytes from in-memory map when
        // already fetched in this session.
        if let Some(cached) = self
            .decrypted_keys
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(url)
        {
            return Ok(cached.clone());
        }

        let rule = rule.expect("rule matched above");
        let mut fetch_url = url.clone();
        if let Some(params) = rule.query_params.as_ref() {
            let mut pairs = fetch_url.query_pairs_mut();
            for (k, v) in params {
                pairs.append_pair(k, v);
            }
        }
        let headers = self.merged_headers(rule.headers.as_ref());

        // Network fetch — no disk cache. The response is encrypted with
        // a fresh per-session seed, so cross-session caching is unsafe.
        let cmd = FetchCmd::get(fetch_url).headers(headers);
        let resp = self.downloader.execute(cmd).await.map_err(HlsError::from)?;
        let raw_key = resp.body.collect().await.map_err(HlsError::from)?;

        let decrypted = rule.processor()(raw_key)
            .map_err(|e| HlsError::KeyProcessing(format!("registry processor: {e}")))?;
        self.decrypted_keys
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(url.clone(), decrypted.clone());
        Ok(decrypted)
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
        let mut buf = kithara_bufpool::byte_pool().get();
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

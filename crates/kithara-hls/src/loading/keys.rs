#![forbid(unsafe_code)]

//! DRM key fetch + processor pipeline.
//!
//! Owns the disk cache + downloader handles directly — no dependency
//! on `FetchManager`. Shares the same atomic-body helper as
//! [`crate::playlist_cache::PlaylistCache`] so the cache lookup,
//! network fetch, and write-back logic is not duplicated.

use bytes::Bytes;
use kithara_assets::AssetStore;
use kithara_drm::{DecryptContext, KeyProcessorRegistry};
use kithara_net::Headers;
use kithara_storage::ResourceExt;
use kithara_stream::dl::PeerHandle;
use url::Url;

use super::atomic_fetch::fetch_atomic_body;
use crate::{HlsError, HlsResult};

/// AES-128 key / IV length in bytes.
const AES_KEY_LEN: usize = 16;

/// Start offset for sequence number in the 16-byte IV.
const IV_SEQUENCE_OFFSET: usize = 8;

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
}

impl KeyManager {
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

    /// Load, optionally preprocess, and return the raw key bytes.
    ///
    /// Looks up the matching rule in [`KeyProcessorRegistry`] by the
    /// key URL's host, applies the rule's query params + headers to
    /// the request, and runs the rule's processor on the response.
    ///
    /// # Errors
    /// Returns an error when the fetch or the processor fails.
    pub async fn get_raw_key(&self, url: &Url, _iv: Option<[u8; AES_KEY_LEN]>) -> HlsResult<Bytes> {
        let rule = self.key_registry.as_ref().and_then(|r| r.find(url));
        let mut fetch_url = url.clone();
        if let Some(r) = rule
            && let Some(params) = r.query_params.as_ref()
        {
            let mut pairs = fetch_url.query_pairs_mut();
            for (key, value) in params {
                pairs.append_pair(key, value);
            }
        }

        let headers = self.merged_headers(rule.and_then(|r| r.headers.as_ref()));
        let rel_path = rel_path_from_url(&fetch_url);
        let raw_key = fetch_atomic_body(
            &self.downloader,
            &self.backend,
            headers,
            &fetch_url,
            rel_path.as_str(),
            "key",
        )
        .await?;

        match rule {
            Some(r) => r.processor()(raw_key)
                .map_err(|e| HlsError::KeyProcessing(format!("registry processor: {e}"))),
            None => Ok(raw_key),
        }
    }

    /// Synchronous cache-only key lookup.
    ///
    /// Reads the raw key bytes from [`AssetStore`] without network I/O.
    /// Returns an error if the key is not in the cache (pre-fetch missed
    /// or evicted).
    ///
    /// # Errors
    /// Returns an error when the key is missing from cache or the read fails.
    pub fn get_cached_key(&self, url: &Url) -> HlsResult<Bytes> {
        let rule = self.key_registry.as_ref().and_then(|r| r.find(url));
        let mut fetch_url = url.clone();
        if let Some(r) = rule
            && let Some(params) = r.query_params.as_ref()
        {
            let mut pairs = fetch_url.query_pairs_mut();
            for (key, value) in params {
                pairs.append_pair(key, value);
            }
        }

        let key = kithara_assets::ResourceKey::from_url(&fetch_url);
        let res = self
            .backend
            .open_resource(&key)
            .map_err(|e| HlsError::KeyProcessing(format!("key not in cache: {fetch_url} — {e}")))?;
        let mut buf = kithara_bufpool::byte_pool().get();
        let n = res.read_into(&mut buf).map_err(|e| {
            HlsError::KeyProcessing(format!("failed to read cached key: {fetch_url} — {e}"))
        })?;
        if n == 0 {
            return Err(HlsError::KeyProcessing(format!(
                "cached key is empty: {fetch_url}",
            )));
        }
        Ok(Bytes::copy_from_slice(&buf[..n]))
    }

    /// Merge per-rule headers on top of base headers.
    /// Rule-specific entries take precedence on key conflict.
    fn merged_headers(
        &self,
        rule_headers: Option<&std::collections::HashMap<String, String>>,
    ) -> Option<Headers> {
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
    ) -> [u8; AES_KEY_LEN] {
        if let Some(iv) = key_info.iv {
            return iv;
        }
        let mut iv = [0u8; AES_KEY_LEN];
        iv[IV_SEQUENCE_OFFSET..].copy_from_slice(&sequence.to_be_bytes());
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

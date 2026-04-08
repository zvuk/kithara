#![forbid(unsafe_code)]

//! DRM key fetch + processor pipeline.
//!
//! Owns the disk cache + downloader handles directly — no dependency
//! on `FetchManager`. Shares the same atomic-body helper as
//! [`crate::playlist_cache::PlaylistCache`] so the cache lookup,
//! network fetch, and write-back logic is not duplicated.

use std::collections::HashMap;

use bytes::Bytes;
use kithara_assets::{AssetStore, AssetsError};
use kithara_drm::DecryptContext;
use kithara_net::Headers;
use kithara_stream::dl::Downloader;
use thiserror::Error;
use url::Url;

use super::atomic_fetch::fetch_atomic_body;
use crate::{HlsError, HlsResult, KeyContext, config::KeyProcessor};

/// AES-128 key / IV length in bytes.
const AES_KEY_LEN: usize = 16;

/// Start offset for sequence number in the 16-byte IV.
const IV_SEQUENCE_OFFSET: usize = 8;

#[derive(Debug, Error)]
pub enum KeyError {
    #[error("Network error: {0}")]
    Net(#[from] kithara_net::NetError),

    #[error("Assets error: {0}")]
    Assets(#[from] AssetsError),

    #[error("Key processing failed: {0}")]
    Processing(String),

    #[error("Invalid URL: {0}")]
    InvalidUrl(String),

    #[error("Key not found: {0}")]
    KeyNotFound(String),
}

/// DRM key fetch + optional processor pipeline.
///
/// Reads through the unified [`Downloader`] and persists key bodies in
/// the supplied [`AssetStore`] via the shared
/// [`fetch_atomic_body`] helper. No dependency on `FetchManager`.
#[derive(Clone)]
pub struct KeyManager {
    downloader: Downloader,
    backend: AssetStore<DecryptContext>,
    /// Cache-wide headers (typically equal to `HlsConfig::headers`).
    base_headers: Option<Headers>,
    key_processor: Option<KeyProcessor>,
    key_query_params: Option<HashMap<String, String>>,
    key_request_headers: Option<HashMap<String, String>>,
}

impl KeyManager {
    #[must_use]
    pub fn new(
        downloader: Downloader,
        backend: AssetStore<DecryptContext>,
        base_headers: Option<Headers>,
        key_processor: Option<KeyProcessor>,
        key_query_params: Option<HashMap<String, String>>,
        key_request_headers: Option<HashMap<String, String>>,
    ) -> Self {
        Self {
            downloader,
            backend,
            base_headers,
            key_processor,
            key_query_params,
            key_request_headers,
        }
    }

    /// Convenience constructor from [`crate::config::KeyOptions`].
    #[must_use]
    pub fn from_options(
        downloader: Downloader,
        backend: AssetStore<DecryptContext>,
        base_headers: Option<Headers>,
        options: crate::config::KeyOptions,
    ) -> Self {
        Self::new(
            downloader,
            backend,
            base_headers,
            options.key_processor,
            options.query_params,
            options.request_headers,
        )
    }

    /// Load, optionally preprocess, and return the raw key bytes.
    ///
    /// Appends configured query parameters, merges key request headers
    /// over the cache-wide base headers, and routes the fetch through
    /// [`fetch_atomic_body`] so the disk cache and unified downloader
    /// are reused.
    ///
    /// # Errors
    /// Returns an error when the fetch or custom processor fails.
    pub async fn get_raw_key(&self, url: &Url, iv: Option<[u8; AES_KEY_LEN]>) -> HlsResult<Bytes> {
        let mut fetch_url = url.clone();
        if let Some(ref params) = self.key_query_params {
            let mut pairs = fetch_url.query_pairs_mut();
            for (key, value) in params {
                pairs.append_pair(key, value);
            }
        }

        let headers = self.merged_headers();
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

        self.process_key(raw_key, fetch_url, iv)
    }

    /// Merge key-specific request headers on top of the base headers.
    /// Key-specific entries take precedence on key conflict.
    fn merged_headers(&self) -> Option<Headers> {
        let key_headers = self.key_request_headers.clone().map(Headers::from);
        match (self.base_headers.clone(), key_headers) {
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

    fn process_key(&self, key: Bytes, url: Url, iv: Option<[u8; AES_KEY_LEN]>) -> HlsResult<Bytes> {
        let context = KeyContext { iv, url };
        if let Some(processor) = &self.key_processor {
            processor(key, context)
        } else {
            Ok(key)
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

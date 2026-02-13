use std::{collections::HashMap, sync::Arc};

use bytes::Bytes;
use kithara_assets::AssetsError;
use kithara_net::Headers;
use thiserror::Error;
use url::Url;

use crate::{HlsError, HlsResult, KeyContext, config::KeyProcessor, fetch::DefaultFetchManager};

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

#[derive(Clone)]
pub struct KeyManager {
    fetch: Arc<DefaultFetchManager>,
    key_processor: Option<KeyProcessor>,
    key_query_params: Option<HashMap<String, String>>,
    key_request_headers: Option<HashMap<String, String>>,
}

impl KeyManager {
    pub fn new(
        fetch: Arc<DefaultFetchManager>,
        key_processor: Option<KeyProcessor>,
        key_query_params: Option<HashMap<String, String>>,
        key_request_headers: Option<HashMap<String, String>>,
    ) -> Self {
        Self {
            fetch,
            key_processor,
            key_query_params,
            key_request_headers,
        }
    }

    /// Create from `KeyOptions` and a shared fetch manager.
    pub fn from_options(
        fetch: Arc<DefaultFetchManager>,
        options: crate::config::KeyOptions,
    ) -> Self {
        Self {
            fetch,
            key_processor: options.key_processor,
            key_query_params: options.query_params,
            key_request_headers: options.request_headers,
        }
    }

    pub async fn get_raw_key(&self, url: &Url, iv: Option<[u8; 16]>) -> HlsResult<Bytes> {
        let mut fetch_url = url.clone();
        if let Some(ref params) = self.key_query_params {
            let mut pairs = fetch_url.query_pairs_mut();
            for (key, value) in params {
                pairs.append_pair(key, value);
            }
        }

        let headers: Option<Headers> = self.key_request_headers.clone().map(Headers::from);
        let rel_path = Self::rel_path_from_url(&fetch_url);
        let raw_key = Arc::clone(&self.fetch)
            .fetch_key(&fetch_url, rel_path.as_str(), headers)
            .await?;

        let processed_key = self.process_key(raw_key, fetch_url, iv)?;
        Ok(processed_key)
    }

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

    fn process_key(&self, key: Bytes, url: Url, iv: Option<[u8; 16]>) -> HlsResult<Bytes> {
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

    pub(crate) fn derive_iv(key_info: &crate::parsing::KeyInfo, sequence: u64) -> [u8; 16] {
        if let Some(iv) = key_info.iv {
            return iv;
        }
        let mut iv = [0u8; 16];
        iv[8..].copy_from_slice(&sequence.to_be_bytes());
        iv
    }
}

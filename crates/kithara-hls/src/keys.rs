use std::{collections::HashMap, sync::Arc};

use bytes::Bytes;
use kithara_assets::AssetsError;
use kithara_net::Headers;
use thiserror::Error;
use url::Url;

use crate::{HlsResult, KeyContext, fetch::FetchManager};

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
    fetch: FetchManager,
    key_processor: Option<Arc<dyn Fn(Bytes, KeyContext) -> HlsResult<Bytes> + Send + Sync>>,
    key_query_params: Option<HashMap<String, String>>,
    key_request_headers: Option<HashMap<String, String>>,
}

impl KeyManager {
    pub fn new(
        fetch: FetchManager,
        key_processor: Option<Arc<dyn Fn(Bytes, KeyContext) -> HlsResult<Bytes> + Send + Sync>>,
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

    pub async fn get_key(&self, url: &Url, iv: Option<[u8; 16]>) -> HlsResult<Bytes> {
        let mut fetch_url = url.clone();

        if let Some(ref params) = self.key_query_params {
            let mut pairs = fetch_url.query_pairs_mut();
            for (key, value) in params {
                pairs.append_pair(key, value);
            }
        }

        let headers: Option<Headers> = self.key_request_headers.clone().map(Headers::from);

        let rel_path = Self::rel_path_from_url(&fetch_url);
        let raw_key = self
            .fetch
            .fetch_key_atomic(&fetch_url, rel_path.as_str(), headers)
            .await?;

        let processed_key = self.process_key(raw_key, fetch_url, iv)?;
        Ok(processed_key)
    }

    fn rel_path_from_url(url: &Url) -> String {
        let last = url
            .path_segments()
            .and_then(|segments| segments.last())
            .unwrap_or("index");
        if let Some((stem, _)) = last.rsplit_once('.') {
            if !stem.is_empty() {
                return stem.to_string();
            }
        }
        last.to_string()
    }

    fn process_key(&self, key: Bytes, url: Url, iv: Option<[u8; 16]>) -> HlsResult<Bytes> {
        let context = KeyContext { url, iv };

        if let Some(processor) = &self.key_processor {
            processor(key, context)
        } else {
            Ok(key)
        }
    }
}

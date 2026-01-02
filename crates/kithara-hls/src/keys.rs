use bytes::Bytes;
use kithara_cache::{AssetCache, CachePath};
use kithara_core::AssetId;
use kithara_net::NetClient;
use std::collections::HashMap;
use thiserror::Error;
use url::Url;

use crate::{HlsError, HlsResult, KeyContext};

#[derive(Debug, Error)]
pub enum KeyError {
    #[error("Network error: {0}")]
    Net(#[from] kithara_net::NetError),

    #[error("Cache error: {0}")]
    Cache(#[from] kithara_cache::CacheError),

    #[error("Key processing failed: {0}")]
    Processing(String),

    #[error("Invalid URL: {0}")]
    InvalidUrl(String),

    #[error("Key not found: {0}")]
    KeyNotFound(String),
}

pub struct KeyManager {
    cache: AssetCache,
    net: NetClient,
    key_processor: Option<Box<dyn Fn(Bytes, KeyContext) -> HlsResult<Bytes> + Send + Sync>>,
    key_query_params: Option<HashMap<String, String>>,
    key_request_headers: Option<HashMap<String, String>>,
}

impl KeyManager {
    pub fn new(
        cache: AssetCache,
        net: NetClient,
        key_processor: Option<Box<dyn Fn(Bytes, KeyContext) -> HlsResult<Bytes> + Send + Sync>>,
        key_query_params: Option<HashMap<String, String>>,
        key_request_headers: Option<HashMap<String, String>>,
    ) -> Self {
        Self {
            cache,
            net,
            key_processor,
            key_query_params,
            key_request_headers,
        }
    }

    pub async fn get_key(&self, url: &Url, iv: Option<[u8; 16]>) -> HlsResult<Bytes> {
        let asset_id = AssetId::from_url(url)?;
        let cache_path = self.cache_path_for_key(url)?;
        let handle = self.cache.asset(asset_id);

        if handle.exists(&cache_path) {
            let file = handle.open(&cache_path)?.unwrap();
            use std::io::Read;
            let mut buf = Vec::new();
            std::io::Read::read_to_end(&mut file, &mut buf).unwrap();
            return Ok(Bytes::from(buf));
        }

        let raw_key = self.fetch_raw_key(url).await?;
        let processed_key = self.process_key(raw_key, url.clone(), iv)?;

        handle.put_atomic(&cache_path, &processed_key)?;
        Ok(processed_key)
    }

    async fn fetch_raw_key(&self, url: &Url) -> HlsResult<Bytes> {
        let mut fetch_url = url.clone();

        if let Some(ref params) = self.key_query_params {
            let mut pairs = fetch_url.query_pairs_mut();
            for (key, value) in params {
                pairs.append_pair(key, value);
            }
        }

        let stream = self
            .net
            .stream(fetch_url, self.key_request_headers.clone())
            .await?;

        use futures::StreamExt;
        let mut bytes = Vec::new();
        let mut pinned_stream = Box::pin(stream);
        while let Some(chunk) = pinned_stream.next().await {
            bytes.extend_from_slice(&chunk?);
        }
        Ok(bytes::Bytes::from(bytes)).map_err(|e: kithara_net::NetError| HlsError::from(e))
    }

    fn process_key(&self, key: Bytes, url: Url, iv: Option<[u8; 16]>) -> HlsResult<Bytes> {
        let context = KeyContext { url, iv };

        if let Some(ref processor) = self.key_processor {
            processor(key, context)
        } else {
            Ok(key)
        }
    }

    fn cache_path_for_key(&self, url: &Url) -> HlsResult<CachePath> {
        let filename = url
            .path_segments()
            .and_then(|segments| segments.last())
            .and_then(|name| if name.is_empty() { None } else { Some(name) })
            .unwrap_or("key");

        let key_name = if let Some(pos) = filename.rfind('.') {
            &filename[..pos]
        } else {
            filename
        };

        CachePath::new(vec![
            "hls".to_string(),
            "keys".to_string(),
            format!("{}.processed", key_name),
        ])
        .map_err(|e| {
            HlsError::from(kithara_cache::CacheError::InvalidPath(format!(
                "Invalid cache path: {}",
                e
            )))
        })
    }
}

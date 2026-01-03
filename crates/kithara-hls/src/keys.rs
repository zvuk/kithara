use bytes::Bytes;
use futures::StreamExt;
use kithara_assets::AssetCache;
use kithara_net::{Headers, HttpClient};
use std::collections::HashMap;
use thiserror::Error;
use url::Url;

use crate::{HlsError, HlsResult, KeyContext};

#[derive(Debug, Error)]
pub enum KeyError {
    #[error("Network error: {0}")]
    Net(#[from] kithara_net::NetError),

    #[error("Assets error: {0}")]
    Cache(#[from] kithara_assets::CacheError),

    #[error("Key processing failed: {0}")]
    Processing(String),

    #[error("Invalid URL: {0}")]
    InvalidUrl(String),

    #[error("Key not found: {0}")]
    KeyNotFound(String),
}

pub struct KeyManager {
    cache: AssetCache,
    net: HttpClient,
    key_processor: Option<Box<dyn Fn(Bytes, KeyContext) -> HlsResult<Bytes> + Send + Sync>>,
    key_query_params: Option<HashMap<String, String>>,
    key_request_headers: Option<HashMap<String, String>>,
}

impl KeyManager {
    pub fn new(
        cache: AssetCache,
        net: HttpClient,
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
        // NOTE: Assets integration is being redesigned to use the new resource-based API
        // (`kithara-assets` + `kithara-storage`).
        //
        // The old cache layer (`kithara-cache`) supported `CachePath` + `put_atomic` and is no
        // longer available here. For now, fetch from the network only.
        let _ = &self.cache;

        let raw_key = self.fetch_raw_key(url).await?;
        let processed_key = self.process_key(raw_key, url.clone(), iv)?;
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

        let headers: Option<Headers> = self.key_request_headers.clone().map(Headers::from);

        let stream = self.net.stream(fetch_url, headers).await?;
        let mut pinned_stream = Box::pin(stream);

        let mut bytes = Vec::new();
        while let Some(chunk) = pinned_stream.next().await {
            let chunk: Bytes = chunk?;
            bytes.extend_from_slice(&chunk);
        }

        Ok(Bytes::from(bytes))
    }

    fn process_key(&self, key: Bytes, url: Url, iv: Option<[u8; 16]>) -> HlsResult<Bytes> {
        let context = KeyContext { url, iv };

        if let Some(ref processor) = self.key_processor {
            processor(key, context)
        } else {
            Ok(key)
        }
    }

    // NOTE: Old cache path logic removed while the new resource-based assets API is being wired in.
}

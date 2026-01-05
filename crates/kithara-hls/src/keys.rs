use std::collections::HashMap;

use bytes::Bytes;
use futures::StreamExt;
use kithara_assets::{AssetStore, ResourceKey};
use kithara_net::{Headers, HttpClient};
use kithara_storage::Resource as _;
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::{HlsError, HlsResult, KeyContext};

fn uri_basename_no_query(uri: &str) -> Option<&str> {
    let no_query = uri.split('?').next().unwrap_or(uri);
    let base = no_query.rsplit('/').next().unwrap_or(no_query);
    if base.is_empty() { None } else { Some(base) }
}

#[derive(Debug, Error)]
pub enum KeyError {
    #[error("Network error: {0}")]
    Net(#[from] kithara_net::NetError),

    #[error("Assets error: {0}")]
    Assets(#[from] kithara_assets::AssetsError),

    #[error("Key processing failed: {0}")]
    Processing(String),

    #[error("Invalid URL: {0}")]
    InvalidUrl(String),

    #[error("Key not found: {0}")]
    KeyNotFound(String),
}

pub struct KeyManager {
    asset_root: String,
    assets: AssetStore,
    net: HttpClient,
    key_processor: Option<Box<dyn Fn(Bytes, KeyContext) -> HlsResult<Bytes> + Send + Sync>>,
    key_query_params: Option<HashMap<String, String>>,
    key_request_headers: Option<HashMap<String, String>>,
}

impl KeyManager {
    pub fn new(
        asset_root: String,
        assets: AssetStore,
        net: HttpClient,
        key_processor: Option<Box<dyn Fn(Bytes, KeyContext) -> HlsResult<Bytes> + Send + Sync>>,
        key_query_params: Option<HashMap<String, String>>,
        key_request_headers: Option<HashMap<String, String>>,
    ) -> Self {
        Self {
            asset_root,
            assets,
            net,
            key_processor,
            key_query_params,
            key_request_headers,
        }
    }

    pub async fn get_key(&self, url: &Url, iv: Option<[u8; 16]>) -> HlsResult<Bytes> {
        // Keys are metadata => AtomicResource.
        //
        // Disk layout contract (stream-download-hls compatible, adapted to kithara-assets):
        // - asset_root = "<master_hash>"
        // - rel_path   = "<variant_id>/<key_basename>"
        //
        // NOTE: Variant id is not plumbed through this API yet; use `0` for now.
        let basename = uri_basename_no_query(url.as_str())
            .ok_or_else(|| HlsError::InvalidUrl("Failed to derive key basename".into()))?;
        let rel_path = format!("{}/{}", 0usize, basename);

        let key = ResourceKey::new(self.asset_root.clone(), rel_path);

        let cancel = CancellationToken::new();
        let res = self.assets.open_atomic_resource(&key, cancel).await?;

        // Best-effort cache read.
        let cached = res.read().await?;
        if !cached.is_empty() {
            let processed = self.process_key(cached, url.clone(), iv)?;
            return Ok(processed);
        }

        // Cache miss => fetch and write atomically.
        let raw_key = self.fetch_raw_key(url).await?;
        res.write(&raw_key).await?;

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

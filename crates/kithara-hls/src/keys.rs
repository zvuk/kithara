use std::{collections::HashMap, sync::Arc};

use aes::Aes128;
use bytes::Bytes;
use cbc::{
    Decryptor,
    cipher::{BlockDecryptMut, KeyIvInit, block_padding::Pkcs7},
};
use kithara_assets::AssetsError;
use kithara_net::Headers;
use thiserror::Error;
use url::Url;

use crate::{HlsError, HlsResult, KeyContext, fetch::FetchManager};

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
    fetch: Arc<FetchManager>,
    key_processor: Option<Arc<dyn Fn(Bytes, KeyContext) -> HlsResult<Bytes> + Send + Sync>>,
    key_query_params: Option<HashMap<String, String>>,
    key_request_headers: Option<HashMap<String, String>>,
}

impl KeyManager {
    pub fn new(
        fetch: Arc<FetchManager>,
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
            .fetch_key_atomic(&fetch_url, rel_path.as_str(), headers)
            .await?;

        let processed_key = self.process_key(raw_key, fetch_url, iv)?;
        Ok(processed_key)
    }

    pub async fn decrypt(
        &self,
        url: &Url,
        iv: Option<[u8; 16]>,
        ciphertext: Bytes,
    ) -> HlsResult<Bytes> {
        let iv = iv.unwrap_or([0u8; 16]);
        let key = self.get_raw_key(url, Some(iv)).await?;
        if key.len() != 16 {
            return Err(HlsError::KeyProcessing(format!(
                "invalid AES-128 key length: {}",
                key.len()
            )));
        }

        let mut buf = ciphertext.to_vec();
        let decryptor = Decryptor::<Aes128>::new((&key[..16]).into(), (&iv).into());
        let plain = decryptor
            .decrypt_padded_mut::<Pkcs7>(&mut buf)
            .map_err(|e| HlsError::KeyProcessing(format!("AES-128 decrypt failed: {e}")))?;
        Ok(Bytes::copy_from_slice(plain))
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
        let context = KeyContext { url, iv };

        if let Some(processor) = &self.key_processor {
            processor(key, context)
        } else {
            Ok(key)
        }
    }
}

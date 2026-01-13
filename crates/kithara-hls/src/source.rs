#![forbid(unsafe_code)]

use std::sync::Arc;

use async_trait::async_trait;
use kithara_assets::{AssetId, AssetStore, ResourceKey};
use kithara_net::{HttpClient, NetOptions};
use url::Url;

use crate::{
    driver::HlsDriver, error::HlsResult, events, fetch, keys::KeyManager, options::HlsOptions,
    playlist::PlaylistManager, session::HlsSession,
};

#[async_trait]
pub trait HlsSourceContract: Send + Sync + 'static {
    async fn open(&self, url: Url, opts: HlsOptions, assets: AssetStore) -> HlsResult<HlsSession>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct HlsSource;

#[async_trait]
impl HlsSourceContract for HlsSource {
    async fn open(&self, url: Url, opts: HlsOptions, assets: AssetStore) -> HlsResult<HlsSession> {
        let asset_id = AssetId::from_url(&url)?;
        let asset_root = ResourceKey::asset_root_for_url(&url);
        let net = HttpClient::new(NetOptions::default());

        let fetch_manager = Arc::new(fetch::FetchManager::new(
            asset_root.clone(),
            assets.clone(),
            net.clone(),
        ));
        let key_processor = opts.key_processor_cb.clone();
        let key_manager = Arc::new(KeyManager::new(
            Arc::clone(&fetch_manager),
            key_processor,
            opts.key_query_params.clone(),
            opts.key_request_headers.clone(),
        ));
        let playlist_manager =
            PlaylistManager::new(Arc::clone(&fetch_manager), opts.base_url.clone());
        let event_emitter = events::EventEmitter::new();

        let driver = HlsDriver::new(
            url.clone(),
            opts.clone(),
            playlist_manager,
            Arc::clone(&fetch_manager),
            Arc::clone(&key_manager),
            event_emitter,
        );

        Ok(HlsSession {
            asset_id,
            master_url: url,
            opts,
            assets,
            driver,
        })
    }
}

impl HlsSource {
    pub async fn open(url: Url, opts: HlsOptions, assets: AssetStore) -> HlsResult<HlsSession> {
        HlsSource.open(url, opts, assets).await
    }
}

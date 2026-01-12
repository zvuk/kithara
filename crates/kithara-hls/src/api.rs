#![forbid(unsafe_code)]

use std::{pin::Pin, sync::Arc, time::Duration};

use async_trait::async_trait;
use bytes::Bytes;
use futures::Stream;
use kithara_assets::{AssetId, AssetStore, ResourceKey};
use kithara_net::HttpClient;
pub use session::HlsSessionSource;
use tokio::sync::broadcast;
use url::Url;

use crate::{
    driver,
    error::HlsResult,
    events::{self, HlsEvent},
    fetch,
    keys::KeyManager,
    playlist, session,
};

#[derive(Clone)]
pub struct HlsOptions {
    pub base_url: Option<Url>,
    pub variant_stream_selector:
        Option<Arc<dyn Fn(&crate::playlist::MasterPlaylist) -> Option<usize> + Send + Sync>>,
    pub abr_initial_variant_index: Option<usize>,
    pub abr_min_buffer_for_up_switch: f32,
    pub abr_down_switch_buffer: f32,
    pub abr_throughput_safety_factor: f32,
    pub abr_up_hysteresis_ratio: f32,
    pub abr_down_hysteresis_ratio: f32,
    pub abr_min_switch_interval: Duration,

    // networking/retry/prefetch knobs (kept for now)
    pub request_timeout: Duration,
    pub max_retries: u32,
    pub retry_base_delay: Duration,
    pub max_retry_delay: Duration,
    pub retry_timeout: Duration,
    pub prefetch_buffer_size: Option<usize>,
    pub live_refresh_interval: Option<Duration>,

    // key processing
    pub key_processor_cb: Option<Arc<dyn Fn(Bytes, KeyContext) -> HlsResult<Bytes> + Send + Sync>>,
    pub key_query_params: Option<std::collections::HashMap<String, String>>,
    pub key_request_headers: Option<std::collections::HashMap<String, String>>,
}

impl Default for HlsOptions {
    fn default() -> Self {
        Self {
            base_url: None,
            variant_stream_selector: None,
            abr_initial_variant_index: Some(0),
            abr_min_buffer_for_up_switch: 10.0,
            abr_down_switch_buffer: 5.0,
            abr_throughput_safety_factor: 1.5,
            abr_up_hysteresis_ratio: 1.3,
            abr_down_hysteresis_ratio: 0.8,
            abr_min_switch_interval: Duration::from_secs(30),

            request_timeout: Duration::from_secs(30),
            max_retries: 3,
            retry_base_delay: Duration::from_millis(100),
            max_retry_delay: Duration::from_secs(5),
            retry_timeout: Duration::from_secs(60),
            prefetch_buffer_size: Some(3),
            live_refresh_interval: None,

            key_processor_cb: None,
            key_query_params: None,
            key_request_headers: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct KeyContext {
    pub url: Url,
    pub iv: Option<[u8; 16]>,
}

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
        let net = HttpClient::new(kithara_net::NetOptions::default());

        let fetch_manager =
            fetch::FetchManager::new(asset_root.clone(), assets.clone(), net.clone());
        let key_processor = opts.key_processor_cb.clone();
        let key_manager = KeyManager::new(
            asset_root.clone(),
            fetch_manager.clone(),
            key_processor,
            opts.key_query_params.clone(),
            opts.key_request_headers.clone(),
        );
        let playlist_manager =
            playlist::PlaylistManager::new(fetch_manager.clone(), opts.base_url.clone());
        let event_emitter = events::EventEmitter::new();

        let driver = driver::HlsDriver::new(
            url.clone(),
            opts.clone(),
            playlist_manager,
            fetch_manager.clone(),
            key_manager.clone(),
            event_emitter,
        );

        Ok(HlsSession {
            asset_id,
            master_url: url,
            opts,
            assets,
            key_manager,
            driver,
        })
    }
}

impl HlsSource {
    /// Convenience associated constructor matching the historical API.
    pub async fn open(url: Url, opts: HlsOptions, assets: AssetStore) -> HlsResult<HlsSession> {
        HlsSource.open(url, opts, assets).await
    }
}

pub struct HlsSession {
    asset_id: AssetId,
    master_url: Url,
    opts: HlsOptions,
    assets: AssetStore,
    key_manager: KeyManager,
    driver: driver::HlsDriver,
}

impl HlsSession {
    pub fn asset_id(&self) -> AssetId {
        self.asset_id
    }

    pub fn stream(&self) -> Pin<Box<dyn Stream<Item = HlsResult<Bytes>> + Send + '_>> {
        Box::pin(self.driver.stream())
    }

    pub fn events(&self) -> broadcast::Receiver<HlsEvent> {
        self.driver.events()
    }

    /// Build an I/O `Source` adapter for this session, suitable for wrapping into
    /// `kithara-stream::io::Reader` (`Read + Seek`) for `rodio::Decoder`.
    pub async fn source(&self) -> HlsResult<HlsSessionSource> {
        let asset_root = ResourceKey::asset_root_for_url(&self.master_url);
        let net = HttpClient::new(kithara_net::NetOptions::default());

        let fetch_manager = fetch::FetchManager::new(asset_root.clone(), self.assets.clone(), net);
        let playlist_manager =
            playlist::PlaylistManager::new(fetch_manager.clone(), self.opts.base_url.clone());

        Ok(HlsSessionSource::new(
            self.master_url.clone(),
            self.opts.clone(),
            playlist_manager,
            fetch_manager,
        ))
    }
}

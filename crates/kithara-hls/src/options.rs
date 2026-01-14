#![forbid(unsafe_code)]

use std::{collections::HashMap, path::PathBuf, sync::Arc, time::Duration};

use bytes::Bytes;
use kithara_assets::EvictConfig;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::{error::HlsResult, playlist::MasterPlaylist};

#[derive(Clone, Debug)]
pub struct KeyContext {
    pub url: Url,
    pub iv: Option<[u8; 16]>,
}

#[derive(Clone)]
pub struct HlsOptions {
    pub base_url: Option<Url>,
    pub variant_stream_selector:
        Option<Arc<dyn Fn(&MasterPlaylist) -> Option<usize> + Send + Sync>>,
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
    pub read_chunk_bytes: u64,
    pub prefetch_buffer_size: Option<usize>,
    pub live_refresh_interval: Option<Duration>,
    // key processing
    pub key_processor_cb: Option<Arc<dyn Fn(Bytes, KeyContext) -> HlsResult<Bytes> + Send + Sync>>,
    pub key_query_params: Option<HashMap<String, String>>,
    pub key_request_headers: Option<HashMap<String, String>>,
    // asset storage
    pub cache_dir: Option<PathBuf>,
    pub evict_config: Option<EvictConfig>,
    pub cancel: Option<CancellationToken>,
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
            read_chunk_bytes: 64 * 1024,
            prefetch_buffer_size: Some(3),
            live_refresh_interval: None,
            key_processor_cb: None,
            key_query_params: None,
            key_request_headers: None,
            cache_dir: None,
            evict_config: None,
            cancel: None,
        }
    }
}

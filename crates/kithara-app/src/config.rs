use std::fmt;

use bon::Builder;
use kithara::{
    assets::{AssetStore, AssetStoreBuilder, BytePool, EvictConfig, FlushHub, StoreOptions},
    audio::analysis::BeatAnalysisConfig,
    bufpool::PcmPool,
    hls::SizeProbeMethod,
    prelude::PlaybackResamplerBackend,
    stream::dl::Downloader,
};
use kithara_drm::KeyProcessorRegistry;
use kithara_platform::{CancelToken, sync::Arc};

use crate::{baked, theme::Palette};

/// Application configuration passed to frontends.
///
/// Shared owners and the downloader are mandatory; product knobs default to
/// values baked at compile time from `app.toml`.
#[derive(Clone, Builder)]
#[builder(state_mod(vis = "pub"))]
#[non_exhaustive]
pub struct AppConfig {
    /// App-wide shared asset store.
    pub asset_store: Arc<AssetStore>,
    /// Shared `AssetStore` flush coordinator for every track.
    pub flush_hub: Arc<FlushHub>,
    /// App-wide shared byte pool for network and cache buffers.
    pub byte_pool: BytePool,
    /// App-wide shared PCM pool for playback and track analysis.
    pub pcm_pool: PcmPool,
    /// App master cancel. Single owner for the whole app subtree; the
    /// queue, player, stores, and UI listener all derive children from
    /// it (see `main.rs`). The chain flag reaches the audio worker and HLS
    /// coord lock-free `is_cancelled()` reads; every subsystem derives its
    /// own [`CancelToken::child`] from this consumer-top master.
    pub shutdown: CancelToken,
    /// Shared HTTP downloader for every track.
    pub downloader: Downloader,
    /// DRM key processing registry.
    #[builder(default = baked::build_baked_drm_registry())]
    pub key_registry: KeyProcessorRegistry,
    /// Color palette for the UI.
    #[builder(default)]
    pub palette: Palette,
    /// HLS size-estimation probe strategy (see
    /// [`kithara::hls::SizeProbeMethod`]).
    #[builder(default = baked::BAKED_SIZE_PROBE_METHOD)]
    pub size_probe_method: SizeProbeMethod,
    /// Log filter directives.
    #[builder(default)]
    pub log_directives: Vec<String>,
    /// Audio file URLs or paths to play.
    #[builder(default = default_tracks())]
    pub tracks: Vec<String>,
    /// Accept invalid TLS certificates. Test servers only.
    #[builder(default = baked::BAKED_SHOULD_ACCEPT_INVALID_CERTS)]
    pub should_accept_invalid_certs: bool,
    /// Crossfade duration in seconds.
    #[builder(default = baked::BAKED_CROSSFADE_SECONDS)]
    pub crossfade_seconds: f32,
    /// Number of EQ bands for the UI.
    #[builder(default = baked::BAKED_EQ_BAND_COUNT)]
    pub eq_band_count: usize,
    /// Source beat-analysis tunables.
    #[builder(default)]
    pub beat_analysis: BeatAnalysisConfig<PlaybackResamplerBackend>,
}

fn default_tracks() -> Vec<String> {
    baked::BAKED_TRACKS
        .iter()
        .map(|s| (*s).to_string())
        .collect()
}

impl fmt::Debug for AppConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AppConfig")
            .field("key_registry", &self.key_registry)
            .field("palette", &self.palette)
            .field("log_directives", &self.log_directives)
            .field("tracks", &self.tracks)
            .field("byte_pool", &self.byte_pool)
            .field("pcm_pool", &self.pcm_pool)
            .field(
                "should_accept_invalid_certs",
                &self.should_accept_invalid_certs,
            )
            .field("crossfade_seconds", &self.crossfade_seconds)
            .field("eq_band_count", &self.eq_band_count)
            .field("beat_analysis", &self.beat_analysis)
            .field("size_probe_method", &self.size_probe_method)
            .finish_non_exhaustive()
    }
}

impl AppConfig {
    /// Create a default config around the injected app-wide owners.
    #[must_use]
    pub fn new(
        downloader: Downloader,
        flush_hub: Arc<FlushHub>,
        cancel: CancelToken,
        byte_pool: BytePool,
        pcm_pool: PcmPool,
    ) -> Self {
        let store_options = StoreOptions::builder()
            .flush_hub(Arc::clone(&flush_hub))
            .build();
        let asset_store = Arc::new(
            AssetStoreBuilder::default()
                .cancel(cancel.child())
                .backend(store_options.backend.clone())
                .evict_config(EvictConfig::from(&store_options))
                .pool(byte_pool.clone())
                .maybe_cache_capacity(store_options.cache_capacity)
                .maybe_flush_hub(store_options.flush_hub)
                .build(),
        );
        Self::builder()
            .downloader(downloader)
            .flush_hub(flush_hub)
            .shutdown(cancel)
            .byte_pool(byte_pool)
            .pcm_pool(pcm_pool)
            .asset_store(asset_store)
            .build()
    }
}

use std::{fmt, sync::Arc};

use bon::Builder;
use kithara::{
    assets::{
        AssetStore, AssetStoreBuilder, BytePool, EvictConfig, FlushHub, PrettyLayout, StoreOptions,
    },
    hls::SizeProbeMethod,
    stream::dl::Downloader,
};
use kithara_drm::KeyProcessorRegistry;
use kithara_platform::CancelToken;

use crate::{baked, theme::Palette};

/// Application configuration passed to frontends.
///
/// Downloader and flush hub are the only mandatory fields; every
/// other knob defaults to the value baked at compile time from
/// `app.toml`. Callers typically do
/// `AppConfig::builder()` and override anything else via the generated
/// builder setters.
#[derive(Clone, Builder)]
#[builder(state_mod(vis = "pub"))]
#[non_exhaustive]
pub struct AppConfig {
    /// App-wide shared asset store.
    pub asset_store: Arc<AssetStore>,
    /// Shared `AssetStore` flush coordinator for every track.
    pub flush_hub: Arc<FlushHub>,
    /// App master cancel. Single owner for the whole app subtree; the
    /// queue, player, stores, and UI listener all derive children from
    /// it (see `main.rs`). The chain flag reaches the audio worker and HLS
    /// coord lock-free `is_cancelled()` reads; every subsystem derives its
    /// own [`CancelToken::child`] from this consumer-top master.
    pub shutdown: CancelToken,
    /// Shared HTTP downloader for every track.
    pub downloader: Downloader,
    /// App-wide shared byte pool for network and cache buffers.
    #[builder(default = BytePool::default())]
    pub byte_pool: BytePool,
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
            .field(
                "should_accept_invalid_certs",
                &self.should_accept_invalid_certs,
            )
            .field("crossfade_seconds", &self.crossfade_seconds)
            .field("eq_band_count", &self.eq_band_count)
            .field("size_probe_method", &self.size_probe_method)
            .finish_non_exhaustive()
    }
}

impl AppConfig {
    /// Create a default config around the given downloader, shared flush
    /// hub, and app master cancel. Builds an app-wide asset store as a
    /// child of `cancel` so every track shares one cache and one download
    /// per URL.
    #[must_use]
    pub fn new(downloader: Downloader, flush_hub: Arc<FlushHub>, cancel: CancelToken) -> Self {
        let byte_pool = BytePool::default();
        let store_options = StoreOptions::builder()
            .flush_hub(Arc::clone(&flush_hub))
            .build();
        let asset_store = Arc::new(
            AssetStoreBuilder::default()
                .cancel(cancel.child())
                .root_dir(&store_options.cache_dir)
                .evict_config(EvictConfig::from(&store_options))
                .ephemeral(store_options.is_ephemeral)
                .layout(Arc::new(PrettyLayout))
                .pool(byte_pool.clone())
                .maybe_cache_capacity(store_options.cache_capacity)
                .maybe_flush_hub(store_options.flush_hub.clone())
                .build(),
        );
        Self::builder()
            .downloader(downloader)
            .flush_hub(flush_hub)
            .shutdown(cancel)
            .byte_pool(byte_pool)
            .asset_store(asset_store)
            .build()
    }
}

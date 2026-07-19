use std::fmt;

use bon::Builder;
use kithara::{
    assets::{AssetStore, AssetStoreBuilder, BytePool, FlushHub, StorageBackend},
    audio::analysis::BeatAnalysisConfig,
    bufpool::PcmPool,
    hls::SizeProbeMethod,
    prelude::PlaybackResamplerBackend,
    stream::dl::Downloader,
};
use kithara_drm::KeyProcessorRegistry;
use kithara_platform::{CancelToken, sync::Arc};

use crate::{baked, theme::Palette};

#[derive(Clone, Copy, Debug, Builder)]
#[builder(state_mod(vis = "pub"))]
#[non_exhaustive]
pub struct WindowSizing {
    /// Fixed horizontal chrome the modular renderer adds around the compiled body.
    #[builder(default = 0.0)]
    pub chrome_w: f32,
    /// Fixed vertical chrome the modular renderer adds around the compiled body.
    #[builder(default = 0.0)]
    pub chrome_h: f32,
    /// Minimum width used when the compiled tree has no intrinsic width.
    #[builder(default = 200.0)]
    pub min_floor_w: f32,
    /// Minimum height used when the compiled tree has no intrinsic height.
    #[builder(default = 140.0)]
    pub min_floor_h: f32,
    /// Scale from an open axis minimum to its comfortable initial size.
    #[builder(default = 1.5)]
    pub initial_scale: f32,
}

impl Default for WindowSizing {
    fn default() -> Self {
        Self::builder().build()
    }
}

/// Application configuration passed to frontends.
///
/// Shared owners and the downloader are mandatory; product knobs default to
/// values baked at compile time from `app.toml`.
#[derive(Clone, Builder)]
#[builder(state_mod(vis = "pub"))]
#[non_exhaustive]
pub struct AppConfig {
    /// App-wide shared asset store.
    pub store: AssetStore,
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
    /// Main-window chrome, floors, and initial-size policy.
    #[builder(default)]
    pub window_sizing: WindowSizing,
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
            .field("window_sizing", &self.window_sizing)
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
        let store = AssetStoreBuilder::default()
            .cancel(cancel.child())
            .backend(StorageBackend::default())
            .pool(byte_pool.clone())
            .flush_hub(flush_hub)
            .build();
        Self::builder()
            .downloader(downloader)
            .shutdown(cancel)
            .byte_pool(byte_pool)
            .pcm_pool(pcm_pool)
            .store(store)
            .build()
    }
}

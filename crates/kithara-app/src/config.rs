use std::{fmt, sync::Arc};

use bon::Builder;
use kithara::{
    assets::{AssetStore, FlushHub, StoreOptions},
    hls::{HlsStore, SizeProbeMethod},
    stream::dl::Downloader,
};
use kithara_drm::KeyProcessorRegistry;
use kithara_platform::CancellationToken;

use crate::{baked, theme::Palette};

/// Application configuration passed to frontends.
///
/// Downloader and flush hub are the only mandatory fields; every
/// other knob defaults to the value baked at compile time from
/// `app.toml`. Callers typically do
/// `AppConfig::new(dl, hub).with_tracks(cli_tracks)` and override
/// anything else via the generated `with_*` setters.
#[derive(Clone, Builder)]
#[builder(state_mod(vis = "pub"))]
#[non_exhaustive]
pub struct AppConfig {
    /// Shared `AssetStore` flush coordinator for every track.
    pub flush_hub: Arc<FlushHub>,
    /// Shared HTTP downloader for every track.
    pub downloader: Downloader,
    /// App master cancel. Single owner for the whole app subtree; the
    /// queue, player, stores, and UI listener all derive children from
    /// it (see `main.rs`). The chain flag reaches the audio worker and HLS
    /// coord lock-free `is_cancelled()` reads; every subsystem derives its
    /// own [`CancellationToken::child_token`] from this consumer-top master.
    pub shutdown: CancellationToken,
    /// App-wide shared file store: concurrent consumers of one URL
    /// (player + waveform) share a single download and cache surface.
    pub file_asset_store: Arc<AssetStore>,
    /// App-wide shared HLS store (shared cache + DRM `process_fn` +
    /// per-`asset_root` eviction routing).
    pub hls_asset_store: HlsStore,
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
    /// hub, and app master cancel. Builds the app-wide file and HLS
    /// stores as children of `cancel` so every track shares one cache
    /// and one download per URL. Every other field defaults to its
    /// `app.toml` baked value; override via the generated `with_*`
    /// setters.
    #[must_use]
    pub fn new(
        downloader: Downloader,
        flush_hub: Arc<FlushHub>,
        cancel: CancellationToken,
    ) -> Self {
        let store_options = StoreOptions::default_builder()
            .flush_hub(Arc::clone(&flush_hub))
            .build();
        let file_asset_store =
            kithara::file::build_shared_asset_store(&store_options, cancel.child_token());
        let hls_asset_store =
            kithara::hls::build_shared_asset_store(&store_options, None, cancel.child_token());
        Self::builder()
            .downloader(downloader)
            .flush_hub(flush_hub)
            .shutdown(cancel)
            .file_asset_store(file_asset_store)
            .hls_asset_store(hls_asset_store)
            .build()
    }

    /// Fluent alias to set `should_accept_invalid_certs`.
    #[must_use]
    pub fn with_should_accept_invalid_certs(mut self, value: bool) -> Self {
        self.should_accept_invalid_certs = value;
        self
    }

    /// Override the default track list. Empty input is ignored.
    #[must_use]
    pub fn with_tracks(mut self, tracks: Vec<String>) -> Self {
        if !tracks.is_empty() {
            self.tracks = tracks;
        }
        self
    }
}

use std::{fmt, sync::Arc};

use bon::Builder;
use kithara::{assets::FlushHub, stream::dl::Downloader};
use kithara_drm::KeyProcessorRegistry;

use crate::{drm, theme::Palette};

/// Application configuration passed to frontends.
///
/// Downloader is the only mandatory field; every other knob has a
/// sensible default, so callers typically do
/// `AppConfig::new(dl).with_tracks(cli_tracks)` and override anything
/// else via the generated `with_*` setters.
#[derive(Clone, Builder)]
#[builder(state_mod(vis = "pub"))]
#[non_exhaustive]
pub struct AppConfig {
    /// Shared `AssetStore` flush coordinator for every track.
    pub flush_hub: Arc<FlushHub>,
    /// Shared HTTP downloader for every track.
    pub downloader: Downloader,
    /// DRM key processing registry.
    #[builder(default = drm::default_zvq_key_registry())]
    pub key_registry: KeyProcessorRegistry,
    /// Color palette for the UI.
    #[builder(default)]
    pub palette: Palette,
    /// Log filter directives.
    #[builder(default)]
    pub log_directives: Vec<String>,
    /// Audio file URLs or paths to play.
    #[builder(default = default_tracks())]
    pub tracks: Vec<String>,
    /// Accept invalid TLS certificates. Test servers only.
    #[builder(default = true)]
    pub should_accept_invalid_certs: bool,
    /// Crossfade duration in seconds.
    #[builder(default = 5.0)]
    pub crossfade_seconds: f32,
    /// Number of EQ bands for the UI.
    #[builder(default = 10)]
    pub eq_band_count: usize,
}

fn default_tracks() -> Vec<String> {
    AppConfig::DEFAULT_TRACKS
        .iter()
        .map(ToString::to_string)
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
            .finish_non_exhaustive()
    }
}

impl AppConfig {
    /// Default crossfade duration in seconds.
    pub const DEFAULT_CROSSFADE_SECONDS: f32 = 5.0;
    /// Default number of EQ bands.
    pub const DEFAULT_EQ_BANDS: usize = 10;

    pub const DEFAULT_TRACKS: &[&str] = &[
        "https://stream.silvercomet.top/track.mp3",
        "https://stream.silvercomet.top/hls/master.m3u8",
        "https://stream.silvercomet.top/drm/master.m3u8",
        "https://cdn-edge.zvq.me/track/streamhq?id=27390231",
        "https://cdn-edge.zvq.me/track/streamhq?id=151585912",
        "https://cdn-edge.zvq.me/track/streamhq?id=125475417",
        "https://ecs-stage-slicer-01.zvq.me/drm/track/95038745_1/master.m3u8",
        "https://ecs-stage-slicer-01.zvq.me/hls/track/176000075_1/master.m3u8",
        "https://ecs-stage-slicer-01.zvq.me/drm/track/176000094_1/master.m3u8",
        "https://ecs-stage-slicer-01.zvq.me/hls/track/176000109_1/master.m3u8",
    ];

    /// Create a default config around the given downloader and shared
    /// flush hub. Tracks default to [`Self::DEFAULT_TRACKS`]; override
    /// via [`Self::with_tracks`].
    #[must_use]
    pub fn new(downloader: Downloader, flush_hub: Arc<FlushHub>) -> Self {
        Self::builder()
            .downloader(downloader)
            .flush_hub(flush_hub)
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

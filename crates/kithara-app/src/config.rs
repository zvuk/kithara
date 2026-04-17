use derivative::Derivative;
use derive_setters::Setters;
use kithara::stream::dl::Downloader;
use kithara_drm::KeyProcessorRegistry;

use crate::{drm, theme::Palette};

/// Application configuration passed to frontends.
///
/// Downloader is the only mandatory field; every other knob has a
/// sensible default, so callers typically do
/// `AppConfig::new(dl).with_tracks(cli_tracks)` and override anything
/// else via the generated `with_*` setters.
#[derive(Clone, Derivative, Setters)]
#[derivative(Debug)]
#[setters(prefix = "with_")]
pub struct AppConfig {
    /// Audio file URLs or paths to play.
    #[setters(skip)]
    pub tracks: Vec<String>,
    /// DRM key processing registry. Populated via
    /// [`drm::make_key_registry`](crate::drm::make_key_registry) or
    /// built directly by the embedding app. Carries the
    /// domain-scoped rules — processor + headers (incl. auth) +
    /// query params — that HLS applies to key fetches.
    pub key_registry: KeyProcessorRegistry,
    /// Crossfade duration in seconds.
    pub crossfade_seconds: f32,
    /// Number of EQ bands for the UI.
    pub eq_band_count: usize,
    /// Log filter directives.
    pub log_directives: Vec<String>,
    /// Color palette for the UI.
    pub palette: Palette,
    /// Accept invalid TLS certificates (self-signed, expired). Test servers only.
    pub danger_accept_invalid_certs: bool,
    /// Shared HTTP downloader for every track. Built once in `main` so
    /// the whole app reuses one HTTP pool and runtime context.
    #[setters(skip)]
    #[derivative(Debug = "ignore")]
    pub downloader: Downloader,
}

impl AppConfig {
    /// Default crossfade duration in seconds.
    pub const DEFAULT_CROSSFADE_SECONDS: f32 = 5.0;
    /// Default number of EQ bands.
    pub const DEFAULT_EQ_BANDS: usize = 3;

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

    /// Create a default config around the given downloader. Tracks default
    /// to [`Self::DEFAULT_TRACKS`]; override via [`Self::with_tracks`].
    #[must_use]
    pub fn new(downloader: Downloader) -> Self {
        Self {
            tracks: Self::DEFAULT_TRACKS
                .iter()
                .map(ToString::to_string)
                .collect(),
            key_registry: drm::default_zvq_key_registry(),
            crossfade_seconds: Self::DEFAULT_CROSSFADE_SECONDS,
            eq_band_count: Self::DEFAULT_EQ_BANDS,
            log_directives: Vec::new(),
            palette: Palette::default(),
            danger_accept_invalid_certs: true,
            downloader,
        }
    }

    /// Override the default track list. Empty input is ignored so CLI
    /// users who don't pass any tracks keep the built-in demo set.
    #[must_use]
    pub fn with_tracks(mut self, tracks: Vec<String>) -> Self {
        if !tracks.is_empty() {
            self.tracks = tracks;
        }
        self
    }
}

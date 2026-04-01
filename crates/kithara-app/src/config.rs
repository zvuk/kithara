use crate::theme::Palette;

/// Application configuration passed to frontends.
#[derive(Clone)]
pub struct AppConfig {
    /// Audio file URLs or paths to play.
    pub tracks: Vec<String>,
    /// Domains that require DRM key cipher processing.
    pub drm_domains: Vec<String>,
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
        "https://ecs-stage-slicer-01.zvq.me/hls/track/95038745_1/master.m3u8",
    ];

    /// Create config with default tracks if none provided.
    #[must_use]
    pub fn with_defaults(tracks: Vec<String>) -> Self {
        let tracks = if tracks.is_empty() {
            Self::DEFAULT_TRACKS
                .iter()
                .map(ToString::to_string)
                .collect()
        } else {
            tracks
        };

        Self {
            tracks,
            drm_domains: vec!["zvq.me".to_string()],
            crossfade_seconds: Self::DEFAULT_CROSSFADE_SECONDS,
            eq_band_count: Self::DEFAULT_EQ_BANDS,
            log_directives: Vec::new(),
            palette: Palette::default(),
            danger_accept_invalid_certs: true,
        }
    }
}

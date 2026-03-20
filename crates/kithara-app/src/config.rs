use crate::theme::Palette;

/// Application configuration passed to frontends.
pub struct AppConfig {
    /// Audio file URLs or paths to play.
    pub tracks: Vec<String>,
    /// Crossfade duration in seconds.
    pub crossfade_seconds: f32,
    /// Number of EQ bands for the UI.
    pub eq_band_count: usize,
    /// Log filter directives.
    pub log_directives: Vec<String>,
    /// Color palette for the UI.
    pub palette: Palette,
}

impl AppConfig {
    /// Default crossfade duration in seconds.
    pub const DEFAULT_CROSSFADE_SECONDS: f32 = 5.0;
    /// Default number of EQ bands.
    pub const DEFAULT_EQ_BANDS: usize = 3;

    pub const FILE_URL_DEFAULT: &str = "https://stream.silvercomet.top/track.mp3";
    pub const HLS_URL_DEFAULT: &str = "https://stream.silvercomet.top/hls/master.m3u8";
    pub const DRM_URL_DEFAULT: &str = "https://stream.silvercomet.top/drm/master.m3u8";

    /// Create config with default tracks if none provided.
    #[must_use]
    pub fn with_defaults(tracks: Vec<String>) -> Self {
        let tracks = if tracks.is_empty() {
            vec![
                Self::FILE_URL_DEFAULT.to_string(),
                Self::HLS_URL_DEFAULT.to_string(),
                Self::DRM_URL_DEFAULT.to_string(),
            ]
        } else {
            tracks
        };

        Self {
            tracks,
            crossfade_seconds: Self::DEFAULT_CROSSFADE_SECONDS,
            eq_band_count: Self::DEFAULT_EQ_BANDS,
            log_directives: Vec::new(),
            palette: Palette::default(),
        }
    }
}

use std::time::Duration;

use derivative::Derivative;
use web_time::Instant;

/// ABR mode selection.
#[derive(Clone, Copy, Debug, Derivative, PartialEq, Eq)]
#[derivative(Default)]
pub enum AbrMode {
    /// Automatic bitrate adaptation (ABR enabled).
    /// Optionally specify initial variant index (defaults to 0).
    #[derivative(Default)]
    Auto(Option<usize>),
    /// Manual variant selection (ABR disabled).
    /// Always use the specified variant index.
    Manual(usize),
}

/// ABR (Adaptive Bitrate) configuration.
#[derive(Clone, Derivative)]
#[derivative(Default, Debug)]
pub struct AbrOptions {
    /// Hysteresis ratio for down-switch.
    #[derivative(Default(value = "0.8"))]
    pub down_hysteresis_ratio: f64,
    /// Buffer level (seconds) that triggers down-switch.
    #[derivative(Default(value = "5.0"))]
    pub down_switch_buffer_secs: f64,
    /// Minimum buffer level (seconds) required for up-switch.
    #[derivative(Default(value = "10.0"))]
    pub min_buffer_for_up_switch_secs: f64,
    /// Minimum interval between variant switches.
    #[derivative(Default(value = "Duration::from_secs(30)"))]
    pub min_switch_interval: Duration,
    /// ABR mode: Auto (adaptive) or Manual (fixed variant).
    pub mode: AbrMode,
    /// Sample window for throughput estimation.
    #[derivative(Default(value = "Duration::from_secs(30)"))]
    pub sample_window: Duration,
    /// Safety factor for throughput estimation (e.g., 1.5 means use 66% of estimated throughput).
    #[derivative(Default(value = "1.5"))]
    pub throughput_safety_factor: f64,
    /// Hysteresis ratio for up-switch (bandwidth must exceed target by this factor).
    #[derivative(Default(value = "1.3"))]
    pub up_hysteresis_ratio: f64,
    /// Available variants for ABR selection.
    /// Set by the streaming layer after parsing playlist.
    #[derivative(Debug(format_with = "fmt_variants_len"))]
    pub variants: Vec<Variant>,
}

fn fmt_variants_len(val: &[Variant], f: &mut std::fmt::Formatter) -> std::fmt::Result {
    write!(f, "{}", val.len())
}

impl AbrOptions {
    /// Get initial variant index based on mode.
    #[must_use]
    pub fn initial_variant(&self) -> usize {
        match self.mode {
            AbrMode::Auto(Some(idx)) | AbrMode::Manual(idx) => idx,
            AbrMode::Auto(None) => 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ThroughputSampleSource {
    Network,
    Cache,
}

#[derive(Clone, Copy, Debug)]
pub struct ThroughputSample {
    pub bytes: u64,
    pub duration: Duration,
    pub at: Instant,
    pub source: ThroughputSampleSource,
    pub content_duration: Option<Duration>,
}

/// Minimal variant information needed for ABR decisions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Variant {
    pub variant_index: usize,
    pub bandwidth_bps: u64,
}

/// Extended variant metadata for UI and monitoring.
///
/// Contains all available information about a variant extracted from
/// the master playlist. This is emitted via `HlsEvent::VariantsDiscovered`
/// after the master playlist is loaded.
#[derive(Clone, Debug)]
pub struct VariantInfo {
    /// Variant index (stable identifier).
    pub index: usize,
    /// Bandwidth in bits per second (if available).
    pub bandwidth_bps: Option<u64>,
    /// Human-readable name (if available).
    pub name: Option<String>,
    /// Codec information (e.g., "avc1.64001f,mp4a.40.2").
    pub codecs: Option<String>,
    /// Container format (MP4, MPEG-TS, etc.).
    pub container: Option<String>,
}

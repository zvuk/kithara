use std::time::{Duration, Instant};

/// ABR mode selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AbrMode {
    /// Automatic bitrate adaptation (ABR enabled).
    /// Optionally specify initial variant index (defaults to 0).
    Auto(Option<usize>),
    /// Manual variant selection (ABR disabled).
    /// Always use the specified variant index.
    Manual(usize),
}

impl Default for AbrMode {
    fn default() -> Self {
        Self::Auto(None)
    }
}

/// ABR (Adaptive Bitrate) configuration.
#[derive(Clone)]
pub struct AbrOptions {
    /// Hysteresis ratio for down-switch.
    pub down_hysteresis_ratio: f64,
    /// Buffer level (seconds) that triggers down-switch.
    pub down_switch_buffer_secs: f64,
    /// Minimum buffer level (seconds) required for up-switch.
    pub min_buffer_for_up_switch_secs: f64,
    /// Minimum interval between variant switches.
    pub min_switch_interval: Duration,
    /// ABR mode: Auto (adaptive) or Manual (fixed variant).
    pub mode: AbrMode,
    /// Sample window for throughput estimation.
    pub sample_window: Duration,
    /// Safety factor for throughput estimation (e.g., 1.5 means use 66% of estimated throughput).
    pub throughput_safety_factor: f64,
    /// Hysteresis ratio for up-switch (bandwidth must exceed target by this factor).
    pub up_hysteresis_ratio: f64,
    /// Available variants for ABR selection.
    /// Set by the streaming layer after parsing playlist.
    pub variants: Vec<Variant>,
}

impl Default for AbrOptions {
    fn default() -> Self {
        Self {
            down_hysteresis_ratio: 0.8,
            down_switch_buffer_secs: 5.0,
            min_buffer_for_up_switch_secs: 10.0,
            min_switch_interval: Duration::from_secs(30),
            mode: AbrMode::default(),
            sample_window: Duration::from_secs(30),
            throughput_safety_factor: 1.5,
            up_hysteresis_ratio: 1.3,
            variants: Vec::new(),
        }
    }
}

impl std::fmt::Debug for AbrOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AbrOptions")
            .field("down_hysteresis_ratio", &self.down_hysteresis_ratio)
            .field("down_switch_buffer_secs", &self.down_switch_buffer_secs)
            .field(
                "min_buffer_for_up_switch_secs",
                &self.min_buffer_for_up_switch_secs,
            )
            .field("min_switch_interval", &self.min_switch_interval)
            .field("mode", &self.mode)
            .field("sample_window", &self.sample_window)
            .field("throughput_safety_factor", &self.throughput_safety_factor)
            .field("up_hysteresis_ratio", &self.up_hysteresis_ratio)
            .field("variants", &self.variants.len())
            .finish()
    }
}

impl AbrOptions {
    /// Get initial variant index based on mode.
    pub fn initial_variant(&self) -> usize {
        match self.mode {
            AbrMode::Auto(Some(idx)) => idx,
            AbrMode::Auto(None) => 0,
            AbrMode::Manual(idx) => idx,
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

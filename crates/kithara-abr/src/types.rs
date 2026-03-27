use std::fmt;

use derivative::Derivative;
use kithara_platform::time::{Duration, Instant};

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

impl From<AbrMode> for usize {
    fn from(mode: AbrMode) -> Self {
        match mode {
            AbrMode::Manual(v) => {
                debug_assert!(v < Self::MAX / 2, "variant index too large");
                v
            }
            AbrMode::Auto(None) => Self::MAX,
            AbrMode::Auto(Some(v)) => {
                debug_assert!(v < Self::MAX / 2, "variant index too large");
                Self::MAX - 1 - v
            }
        }
    }
}

impl From<usize> for AbrMode {
    fn from(val: usize) -> Self {
        if val == usize::MAX {
            Self::Auto(None)
        } else if val >= usize::MAX / 2 {
            Self::Auto(Some(usize::MAX - 1 - val))
        } else {
            Self::Manual(val)
        }
    }
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
    /// Maximum bandwidth (bps) for variant selection.
    ///
    /// When set, variants with `bandwidth_bps` exceeding this value are excluded
    /// from ABR decisions. Maps to the `preferredPeakBitRate` concept from
    /// `AVPlayer`. `None` means no limit.
    pub max_bandwidth_bps: Option<u64>,
    /// Minimum buffer level (seconds) required for up-switch.
    #[derivative(Default(value = "10.0"))]
    pub min_buffer_for_up_switch_secs: f64,
    /// Minimum interval between variant switches.
    #[derivative(Default(value = "Duration::from_secs(30)"))]
    pub min_switch_interval: Duration,
    /// Minimum download duration (ms) to record a throughput sample.
    /// Downloads faster than this are ignored (too short for a reliable
    /// estimate). Set to 0 for tests with local servers.
    #[derivative(Default(value = "10"))]
    pub min_throughput_record_ms: u128,
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

fn fmt_variants_len(val: &[Variant], f: &mut fmt::Formatter) -> fmt::Result {
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

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    #[case(AbrMode::Auto(None))]
    #[case(AbrMode::Auto(Some(0)))]
    #[case(AbrMode::Auto(Some(5)))]
    #[case(AbrMode::Auto(Some(42)))]
    #[case(AbrMode::Manual(0))]
    #[case(AbrMode::Manual(1))]
    #[case(AbrMode::Manual(99))]
    fn abr_mode_usize_round_trip(#[case] mode: AbrMode) {
        let encoded: usize = mode.into();
        let decoded: AbrMode = encoded.into();
        assert_eq!(decoded, mode);
    }

    #[kithara::test]
    fn manual_and_auto_encode_differently() {
        let manual: usize = AbrMode::Manual(0).into();
        let auto: usize = AbrMode::Auto(None).into();
        assert_ne!(manual, auto);
    }
}

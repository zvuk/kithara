#![forbid(unsafe_code)]

//! Shared ABR (adaptive bitrate) vocabulary and events.
//!
//! Types here are the single source of truth for the ABR domain — the
//! `kithara-abr` crate depends on this module for shared shapes and
//! publishes [`AbrEvent`] into peer-scoped event buses.

use std::num::NonZeroU64;

use derivative::Derivative;
use kithara_platform::time::Duration;

/// Threshold separating Manual (below) from Auto (at or above) in the packed
/// `usize` representation of [`AbrMode`].
const ABR_MODE_AUTO_THRESHOLD: usize = usize::MAX / 2;

/// ABR mode selection.
#[derive(Clone, Copy, Debug, Derivative, PartialEq, Eq)]
#[derivative(Default)]
pub enum AbrMode {
    /// Automatic bitrate adaptation.
    /// Optional initial variant index (defaults to 0 when `None`).
    #[derivative(Default)]
    Auto(Option<usize>),
    /// Manual variant selection — ABR disabled, fixed variant.
    Manual(usize),
}

impl From<AbrMode> for usize {
    fn from(mode: AbrMode) -> Self {
        match mode {
            AbrMode::Manual(v) => {
                debug_assert!(v < ABR_MODE_AUTO_THRESHOLD, "variant index too large");
                v
            }
            AbrMode::Auto(None) => Self::MAX,
            AbrMode::Auto(Some(v)) => {
                debug_assert!(v < ABR_MODE_AUTO_THRESHOLD, "variant index too large");
                Self::MAX - 1 - v
            }
        }
    }
}

impl From<usize> for AbrMode {
    fn from(val: usize) -> Self {
        if val == usize::MAX {
            Self::Auto(None)
        } else if val >= ABR_MODE_AUTO_THRESHOLD {
            Self::Auto(Some(usize::MAX - 1 - val))
        } else {
            Self::Manual(val)
        }
    }
}

/// Reason attached to an ABR decision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum AbrReason {
    Initial,
    ManualOverride,
    UpSwitch,
    DownSwitch,
    MinInterval,
    NoEstimate,
    /// Bandwidth samples below warmup threshold — no switching yet.
    Warmup,
    BufferTooLowForUpSwitch,
    /// Buffer ahead below urgent-threshold; force down-switch.
    UrgentDownSwitch,
    AlreadyOptimal,
    Locked,
}

/// Source of a bandwidth sample.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum BandwidthSource {
    Network,
    Cache,
}

/// Variant known to the ABR controller (bandwidth + duration shape).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AbrVariant {
    pub variant_index: usize,
    pub bandwidth_bps: u64,
    pub duration: VariantDuration,
}

/// Duration shape for an `AbrVariant`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VariantDuration {
    /// Single total duration (e.g. MP3, WAV).
    Total(Duration),
    /// Per-segment durations (HLS).
    Segmented(Vec<Duration>),
    /// Live / unknown-length stream.
    Unknown,
}

/// Progress snapshot pulled from a peer for buffer-aware ABR decisions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AbrProgressSnapshot {
    pub reader_playback_time: Duration,
    pub download_head_playback_time: Duration,
}

/// Opaque peer identifier assigned by the ABR controller on `register`.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct AbrPeerId(NonZeroU64);

impl AbrPeerId {
    /// Construct from a non-zero identifier.
    #[must_use]
    pub fn new(id: NonZeroU64) -> Self {
        Self(id)
    }

    /// Raw numeric value.
    #[must_use]
    pub fn get(self) -> NonZeroU64 {
        self.0
    }
}

/// ABR controller settings.
#[derive(Clone, Debug, Derivative, PartialEq)]
#[derivative(Default)]
pub struct AbrSettings {
    /// Number of bytes that must be downloaded before ABR will switch.
    #[derivative(Default(value = "128 * 1024"))]
    pub warmup_min_bytes: u64,
    /// Minimum buffer-ahead required before an up-switch is allowed.
    #[derivative(Default(value = "Duration::from_secs(10)"))]
    pub min_buffer_for_up_switch: Duration,
    /// Buffer-ahead at or below this threshold forces an urgent down-switch.
    #[derivative(Default(value = "Duration::from_secs(5)"))]
    pub urgent_downswitch_buffer: Duration,
    /// Minimum interval between variant switches.
    #[derivative(Default(value = "Duration::from_secs(30)"))]
    pub min_switch_interval: Duration,
    /// Safety factor applied to the throughput estimate before comparing to
    /// candidate variants (e.g. `1.5` uses ~66% of the raw estimate).
    #[derivative(Default(value = "1.5"))]
    pub throughput_safety_factor: f64,
    /// Hysteresis ratio for up-switch (adjusted throughput must exceed
    /// candidate bandwidth by this factor).
    #[derivative(Default(value = "1.3"))]
    pub up_hysteresis_ratio: f64,
    /// Hysteresis ratio for down-switch.
    #[derivative(Default(value = "0.8"))]
    pub down_hysteresis_ratio: f64,
    /// Minimum download duration (ms) to record a bandwidth sample — fetches
    /// faster than this are ignored.
    #[derivative(Default(value = "10"))]
    pub min_throughput_record_ms: u128,
    /// Global data-saver cap. Per-peer overrides live in `AbrState`.
    pub max_bandwidth_bps: Option<u64>,
    /// Deadline for the incoherence watcher spawned after a variant switch.
    #[derivative(Default(value = "Duration::from_secs(5)"))]
    pub incoherence_deadline: Duration,
    /// Minimum interval between `AbrEvent::BandwidthEstimate` emits.
    #[derivative(Default(value = "Duration::from_secs(1)"))]
    pub bandwidth_emit_min_interval: Duration,
    /// Minimum relative delta (0.0–1.0) between `BandwidthEstimate` emits.
    #[derivative(Default(value = "0.10"))]
    pub bandwidth_emit_min_delta_ratio: f64,
    /// Minimum interval between `AbrEvent::BufferAhead` emits.
    #[derivative(Default(value = "Duration::from_millis(500)"))]
    pub buffer_emit_min_interval: Duration,
    /// Minimum absolute delta between `BufferAhead` emits.
    #[derivative(Default(value = "Duration::from_millis(500)"))]
    pub buffer_emit_min_delta: Duration,
}

/// Outcome of an ABR decision step.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AbrDecision {
    pub target_variant_index: usize,
    pub reason: AbrReason,
    pub changed: bool,
}

/// Extended variant metadata for UI and monitoring.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VariantInfo {
    pub index: usize,
    pub bandwidth_bps: Option<u64>,
    pub name: Option<String>,
    pub codecs: Option<String>,
    pub container: Option<String>,
}

/// Events emitted by the ABR controller for a single registered peer.
///
/// Published into the peer's track-scoped bus; root-level subscribers see
/// events for every track, track-scoped subscribers only their own.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum AbrEvent {
    // ─── Operational ───
    ThroughputSample {
        bytes_per_second: f64,
        source: BandwidthSource,
    },
    BandwidthEstimate {
        bps: u64,
    },
    BufferAhead {
        ahead: Option<Duration>,
    },

    // ─── State transitions ───
    VariantsRegistered {
        variants: Vec<VariantInfo>,
        initial: usize,
    },
    VariantApplied {
        from: usize,
        to: usize,
        reason: AbrReason,
    },
    ModeChanged {
        mode: AbrMode,
    },
    MaxBandwidthCapChanged {
        cap: Option<u64>,
    },
    Locked,
    Unlocked,

    // ─── Diagnostics ───
    DecisionSkipped {
        reason: AbrReason,
    },
    WarmupCompleted,
    /// Reader did not advance within `incoherence_deadline` after a
    /// `VariantApplied` event. Signals a potential deadlock between the
    /// scheduler and the reader.
    Incoherence {
        description: String,
        elapsed: Duration,
    },
}

#[cfg(test)]
mod tests {
    use kithara_platform::time::Duration;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::Event;

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

    #[kithara::test]
    fn abr_settings_default_values() {
        let s = AbrSettings::default();
        assert_eq!(s.warmup_min_bytes, 128 * 1024);
        assert_eq!(s.min_buffer_for_up_switch, Duration::from_secs(10));
        assert_eq!(s.urgent_downswitch_buffer, Duration::from_secs(5));
        assert_eq!(s.min_switch_interval, Duration::from_secs(30));
        assert!((s.throughput_safety_factor - 1.5).abs() < f64::EPSILON);
        assert!((s.up_hysteresis_ratio - 1.3).abs() < f64::EPSILON);
        assert!((s.down_hysteresis_ratio - 0.8).abs() < f64::EPSILON);
        assert_eq!(s.min_throughput_record_ms, 10);
        assert!(s.max_bandwidth_bps.is_none());
        assert_eq!(s.incoherence_deadline, Duration::from_secs(5));
        assert_eq!(s.bandwidth_emit_min_interval, Duration::from_secs(1));
        assert!((s.bandwidth_emit_min_delta_ratio - 0.10).abs() < f64::EPSILON);
        assert_eq!(s.buffer_emit_min_interval, Duration::from_millis(500));
        assert_eq!(s.buffer_emit_min_delta, Duration::from_millis(500));
    }

    #[kithara::test]
    fn variant_duration_equality() {
        assert_eq!(
            VariantDuration::Total(Duration::from_secs(30)),
            VariantDuration::Total(Duration::from_secs(30)),
        );
        assert_eq!(
            VariantDuration::Segmented(vec![Duration::from_secs(10); 3]),
            VariantDuration::Segmented(vec![Duration::from_secs(10); 3]),
        );
        assert_eq!(VariantDuration::Unknown, VariantDuration::Unknown);
        assert_ne!(
            VariantDuration::Total(Duration::from_secs(5)),
            VariantDuration::Unknown,
        );
    }

    #[kithara::test]
    fn abr_event_into_event_locked() {
        let event: Event = AbrEvent::Locked.into();
        assert!(matches!(event, Event::Abr(AbrEvent::Locked)));
    }

    #[kithara::test]
    fn abr_event_into_event_variant_applied() {
        let event: Event = AbrEvent::VariantApplied {
            from: 0,
            to: 1,
            reason: AbrReason::UpSwitch,
        }
        .into();
        assert!(matches!(
            event,
            Event::Abr(AbrEvent::VariantApplied {
                from: 0,
                to: 1,
                reason: AbrReason::UpSwitch,
            })
        ));
    }

    #[kithara::test]
    fn abr_peer_id_round_trip() {
        let raw = NonZeroU64::new(42).unwrap();
        let id = AbrPeerId::new(raw);
        assert_eq!(id.get(), raw);
    }
}

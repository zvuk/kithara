#![forbid(unsafe_code)]

//! Shared ABR (adaptive bitrate) vocabulary and events.
//!
//! Cross-crate types live here (those referenced by [`AbrEvent`]). Controller-
//! internal shapes (`AbrSettings`, `AbrDecision`, `AbrPeerId`) live in
//! `kithara-abr`; events only carry `AbrMode`, `AbrReason`, `BandwidthSource`,
//! `AbrVariant`, `VariantDuration`, `VariantInfo`, `AbrProgressSnapshot`.

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
}

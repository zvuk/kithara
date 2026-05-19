#![forbid(unsafe_code)]

use kithara_platform::time::Duration;

/// Threshold separating Manual (below) from Auto (at or above) in the packed
/// `usize` representation of [`AbrMode`].
const ABR_MODE_AUTO_THRESHOLD: usize = usize::MAX / 2;

/// ABR mode selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AbrMode {
    /// Automatic bitrate adaptation.
    /// Optional initial variant index (defaults to 0 when `None`).
    Auto(Option<usize>),
    /// Manual variant selection — ABR disabled, fixed variant.
    Manual(usize),
}

impl Default for AbrMode {
    fn default() -> Self {
        Self::Auto(None)
    }
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

/// Duration shape for a variant.
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
    pub download_head_playback_time: Duration,
    pub reader_playback_time: Duration,
}

/// Variant metadata. Single source of truth across HLS parsing, ABR
/// scheduler, event payload, and UI surfaces. Replaces the historical
/// split between `AbrVariant` (bandwidth + duration) and a separate
/// `VariantInfo` for UI metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VariantInfo {
    pub bandwidth_bps: Option<u64>,
    pub codecs: Option<String>,
    pub container: Option<String>,
    pub name: Option<String>,
    pub duration: VariantDuration,
    pub variant_index: usize,
}

/// Events emitted by the ABR controller for a single registered peer.
///
/// Published into the peer's track-scoped bus; root-level subscribers see
/// events for every track, track-scoped subscribers only their own.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum AbrEvent {
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
    DecisionSkipped {
        reason: AbrReason,
    },
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

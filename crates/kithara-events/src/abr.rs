#![forbid(unsafe_code)]

use kithara_platform::time::Duration;

/// Threshold separating Manual (below) from Auto (at or above) in the packed
/// `usize` representation of [`AbrMode`].
const ABR_MODE_AUTO_THRESHOLD: usize = usize::MAX / 2;

/// A validated position into a peer's variant list.
///
/// Keeps a variant position from being confused with a byte offset, a
/// segment index, or an index into a different variant list. Construct via
/// [`VariantIndex::try_new`] at trust boundaries (FFI, UI), or
/// [`VariantIndex::new`] when validity is already structurally guaranteed
/// (atomic reload, playlist parse). See `kithara-abr/CONTEXT.md`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[repr(transparent)]
pub struct VariantIndex(usize);

impl VariantIndex {
    /// Wrap an index whose validity is already guaranteed by construction
    /// (atomic reload, playlist id, test literal). No bounds check.
    #[must_use]
    pub const fn new(idx: usize) -> Self {
        Self(idx)
    }

    #[must_use]
    pub const fn get(self) -> usize {
        self.0
    }

    /// Validated constructor: `Ok` iff `idx < available`.
    ///
    /// # Errors
    /// Returns [`BoundsError`] when `idx >= available`.
    pub fn try_new(idx: usize, available: usize) -> Result<Self, BoundsError> {
        if idx < available {
            Ok(Self(idx))
        } else {
            Err(BoundsError {
                available,
                requested: idx,
            })
        }
    }
}

impl std::fmt::Display for VariantIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

/// A variant index out of range against a known variant count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BoundsError {
    pub available: usize,
    pub requested: usize,
}

impl std::fmt::Display for BoundsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "variant index {} out of bounds (available: {})",
            self.requested, self.available
        )
    }
}

impl std::error::Error for BoundsError {}

/// ABR mode selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AbrMode {
    /// Automatic bitrate adaptation.
    /// Optional initial variant index (defaults to 0 when `None`).
    Auto(Option<VariantIndex>),
    /// Manual variant selection — ABR disabled, fixed variant.
    Manual(VariantIndex),
}

impl Default for AbrMode {
    fn default() -> Self {
        Self::Auto(None)
    }
}

impl AbrMode {
    /// Manual mode pinned to variant `idx`. Shorthand for
    /// `Manual(VariantIndex::new(idx))` — the index is wrapped without a
    /// bounds check; validate at trust boundaries via
    /// [`VariantIndex::try_new`] / `AbrHandle::set_mode`.
    #[must_use]
    pub const fn manual(idx: usize) -> Self {
        Self::Manual(VariantIndex::new(idx))
    }
}

impl From<AbrMode> for usize {
    fn from(mode: AbrMode) -> Self {
        match mode {
            AbrMode::Manual(v) => {
                debug_assert!(v.get() < ABR_MODE_AUTO_THRESHOLD, "variant index too large");
                v.get()
            }
            AbrMode::Auto(None) => Self::MAX,
            AbrMode::Auto(Some(v)) => {
                debug_assert!(v.get() < ABR_MODE_AUTO_THRESHOLD, "variant index too large");
                Self::MAX - 1 - v.get()
            }
        }
    }
}

impl From<usize> for AbrMode {
    fn from(val: usize) -> Self {
        if val == usize::MAX {
            Self::Auto(None)
        } else if val >= ABR_MODE_AUTO_THRESHOLD {
            Self::Auto(Some(VariantIndex::new(usize::MAX - 1 - val)))
        } else {
            Self::Manual(VariantIndex::new(val))
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
    pub variant_index: VariantIndex,
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
        initial: VariantIndex,
    },
    VariantApplied {
        from: VariantIndex,
        to: VariantIndex,
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
    #[case(AbrMode::Auto(Some(VariantIndex::new(0))))]
    #[case(AbrMode::Auto(Some(VariantIndex::new(5))))]
    #[case(AbrMode::Auto(Some(VariantIndex::new(42))))]
    #[case(AbrMode::Manual(VariantIndex::new(0)))]
    #[case(AbrMode::Manual(VariantIndex::new(1)))]
    #[case(AbrMode::Manual(VariantIndex::new(99)))]
    fn abr_mode_usize_round_trip(#[case] mode: AbrMode) {
        let encoded: usize = mode.into();
        let decoded: AbrMode = encoded.into();
        assert_eq!(decoded, mode);
    }

    #[kithara::test]
    fn manual_and_auto_encode_differently() {
        let manual: usize = AbrMode::Manual(VariantIndex::new(0)).into();
        let auto: usize = AbrMode::Auto(None).into();
        assert_ne!(manual, auto);
    }

    #[kithara::test]
    fn variant_index_try_new_accepts_in_range() {
        assert_eq!(VariantIndex::try_new(0, 3), Ok(VariantIndex::new(0)));
        assert_eq!(VariantIndex::try_new(2, 3), Ok(VariantIndex::new(2)));
    }

    #[kithara::test]
    #[case(3, 3)]
    #[case(4, 3)]
    #[case(usize::MAX, 3)]
    fn variant_index_try_new_rejects_out_of_range(#[case] idx: usize, #[case] available: usize) {
        assert_eq!(
            VariantIndex::try_new(idx, available),
            Err(BoundsError {
                requested: idx,
                available,
            })
        );
    }

    #[kithara::test]
    fn variant_index_try_new_against_empty_list_always_fails() {
        assert!(VariantIndex::try_new(0, 0).is_err());
    }

    #[kithara::test]
    fn variant_index_get_round_trips() {
        assert_eq!(VariantIndex::new(7).get(), 7);
    }

    #[kithara::test]
    fn variant_index_display_is_the_bare_index() {
        assert_eq!(format!("{}", VariantIndex::new(42)), "42");
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
            from: VariantIndex::new(0),
            to: VariantIndex::new(1),
            reason: AbrReason::UpSwitch,
        }
        .into();
        let Event::Abr(AbrEvent::VariantApplied { from, to, reason }) = event else {
            panic!("expected VariantApplied");
        };
        assert_eq!(from, VariantIndex::new(0));
        assert_eq!(to, VariantIndex::new(1));
        assert_eq!(reason, AbrReason::UpSwitch);
    }
}

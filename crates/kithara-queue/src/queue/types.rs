use std::sync::atomic::{AtomicU64, Ordering};

use kithara_events::TrackId;
use kithara_play::{PlaybackSnapshot, ResourceSrc};

use crate::track::TrackSource;

/// One-shot view of the player's live playback state, returned by
/// [`Queue::playback_view`](super::Queue::playback_view).
///
/// Collapses position / duration / buffered / playing into a single
/// read so pollers (the FFI time thread, `snapshot`) assemble the whole
/// state from one coherent call instead of several separate accessors.
/// Each field keeps its own `Option` semantics: `position` carries the
/// cached smoothing (filters transient `0.0` on pause/resume), and
/// `duration` is `None` while unknown.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct PlaybackView {
    /// Decoded-ahead buffered/playable seconds; `None` when no track is
    /// active. Always `>=` `position`.
    pub buffered: Option<f64>,
    /// Total media duration in seconds; `None` while unknown.
    pub duration: Option<f64>,
    /// Smoothed playback position in seconds; `None` until a stable
    /// position is known (e.g. between tracks).
    pub position: Option<f64>,
    /// Whether playback is active.
    pub playing: bool,
}

impl From<PlaybackSnapshot> for PlaybackView {
    /// Resolve a raw player snapshot into the queue-level view: an unknown
    /// duration (`0.0`) collapses to `None`, and the decoded `frontier`
    /// becomes the buffered window. `position` is carried through raw here —
    /// [`Queue::playback_view`](super::Queue::playback_view) then overrides
    /// it with the cached/smoothed value it owns.
    fn from(snapshot: PlaybackSnapshot) -> Self {
        Self {
            position: Some(snapshot.position()),
            duration: (snapshot.duration() > 0.0).then_some(snapshot.duration()),
            buffered: Some(snapshot.frontier()),
            playing: snapshot.is_playing(),
        }
    }
}

/// Transition style for a track switch.
///
/// Mirrors the Apple-idiomatic pattern of a namespace-style type with
/// variants describing "what" — not "how" — so the same method
/// signature handles both manual and auto-advance cases.
///
/// - [`Transition::None`] — immediate cut (0 seconds). Matches
///   `AVQueuePlayer`'s user-initiated selection idiom.
/// - [`Transition::Crossfade`] — use the player's configured
///   [`PlayerImpl::crossfade_duration`](kithara_play::PlayerImpl::crossfade_duration).
/// - [`Transition::CrossfadeWith`] — explicit override in seconds.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub enum Transition {
    /// No crossfade; immediate cut.
    None,
    /// Use the player's configured crossfade duration.
    Crossfade,
    /// Use an explicit crossfade duration (seconds).
    CrossfadeWith { seconds: f32 },
}

impl Transition {
    /// Resolve the transition to an actual crossfade duration in
    /// seconds using `default` for [`Transition::Crossfade`].
    #[must_use]
    pub fn crossfade_seconds(self, default: f32) -> f32 {
        match self {
            Self::None => 0.0,
            Self::Crossfade => default,
            Self::CrossfadeWith { seconds } => seconds,
        }
    }
}

/// A pending-select entry: a track id waiting to be applied plus the
/// [`Transition`] the caller asked for. Stored until loading finishes.
#[derive(Clone, Copy, Debug)]
pub(super) struct PendingSelect {
    pub(super) id: TrackId,
    pub(super) transition: Transition,
}

/// Crossfade-arm coordination state. Replaces the `u64::MAX` sentinel
/// previously stored in `crossfade_armed_for`; "no track armed" is the
/// explicit [`CrossfadeArm::Disarmed`] variant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CrossfadeArm {
    Disarmed,
    Armed { for_track: TrackId },
}

impl CrossfadeArm {
    pub(super) fn armed(for_track: TrackId) -> Self {
        Self::Armed { for_track }
    }

    pub(super) fn is_armed_for(self, id: TrackId) -> bool {
        matches!(self, Self::Armed { for_track } if for_track == id)
    }
}

/// Pending-select phase. Replaces `Option<PendingSelect>` where `None`
/// conflated "idle" with "absent"; [`SelectPhase::Idle`] makes the
/// no-selection state explicit.
#[derive(Clone, Copy, Debug)]
pub(super) enum SelectPhase {
    Idle,
    Pending(PendingSelect),
}

/// Cached monotonic playback position. Replaces the `f64::NAN` sentinel
/// stored in `cached_position`; "no value yet" is the explicit
/// [`CachedPosition::Unknown`] variant.
#[derive(Clone, Copy, Debug)]
pub(super) enum CachedPosition {
    Unknown,
    Known { seconds: f64 },
}

impl CachedPosition {
    /// Build a [`CachedPosition::Known`], canonicalising a `NaN` input to
    /// [`CachedPosition::Unknown`] so the type never carries a `NaN`.
    pub(super) fn known(seconds: f64) -> Self {
        if seconds.is_nan() {
            Self::Unknown
        } else {
            Self::Known { seconds }
        }
    }
}

impl From<CachedPosition> for Option<f64> {
    fn from(pos: CachedPosition) -> Self {
        match pos {
            CachedPosition::Known { seconds } => Some(seconds),
            CachedPosition::Unknown => None,
        }
    }
}

/// Lock-free [`CrossfadeArm`] cell for the `tick` hot path. The
/// `u64::MAX` bit pattern encodes [`CrossfadeArm::Disarmed`]; real ids
/// are allocated monotonically from `0`, so the top of the range is
/// free as the sentinel. Orderings match the original raw-`AtomicU64`
/// accessors: `Acquire` load, `Release` store, `AcqRel` swap /
/// compare-exchange.
pub(super) struct AtomicTrackId(AtomicU64);

impl AtomicTrackId {
    const NONE_BITS: u64 = u64::MAX;

    /// CAS [`CrossfadeArm::Disarmed`] → `Armed(track)`. Returns `true`
    /// when this call performed the arm. Used only by the cfg-gated
    /// autoplay path (`register_for_test`).
    #[cfg(any(test, feature = "probe"))]
    pub(super) fn arm_if_disarmed(&self, track: TrackId) -> bool {
        self.0
            .compare_exchange(
                Self::NONE_BITS,
                track.as_u64(),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    fn decode(bits: u64) -> CrossfadeArm {
        if bits == Self::NONE_BITS {
            CrossfadeArm::Disarmed
        } else {
            CrossfadeArm::Armed {
                for_track: TrackId(bits),
            }
        }
    }

    /// CAS `Armed(track)` → [`CrossfadeArm::Disarmed`]. Returns `true`
    /// when `track` was the armed id. Used only by the cfg-gated
    /// autoplay path (`complete_load_for_test`).
    #[cfg(any(test, feature = "probe"))]
    pub(super) fn disarm_if_matches(&self, track: TrackId) -> bool {
        self.0
            .compare_exchange(
                track.as_u64(),
                Self::NONE_BITS,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    pub(super) fn disarmed() -> Self {
        Self(AtomicU64::new(Self::NONE_BITS))
    }

    fn encode(arm: CrossfadeArm) -> u64 {
        match arm {
            CrossfadeArm::Disarmed => Self::NONE_BITS,
            CrossfadeArm::Armed { for_track } => for_track.as_u64(),
        }
    }

    pub(super) fn load(&self) -> CrossfadeArm {
        Self::decode(self.0.load(Ordering::Acquire))
    }

    pub(super) fn store(&self, arm: CrossfadeArm) {
        self.0.store(Self::encode(arm), Ordering::Release);
    }

    pub(super) fn take_if_matches(&self, track: TrackId) -> bool {
        self.0
            .compare_exchange(
                track.as_u64(),
                Self::NONE_BITS,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }
}

/// Lock-free [`CachedPosition`] cell for the `tick` hot path. The
/// `f64::NAN` bit pattern encodes [`CachedPosition::Unknown`]; any `NaN`
/// observed on load (including a `NaN` written through `store`)
/// canonicalises back to `Unknown`.
pub(super) struct AtomicCachedPosition(AtomicU64);

impl AtomicCachedPosition {
    pub(super) fn load(&self) -> CachedPosition {
        let seconds = f64::from_bits(self.0.load(Ordering::Acquire));
        if seconds.is_nan() {
            CachedPosition::Unknown
        } else {
            CachedPosition::Known { seconds }
        }
    }

    pub(super) fn store(&self, pos: CachedPosition) {
        let bits = match pos {
            CachedPosition::Unknown => f64::NAN.to_bits(),
            CachedPosition::Known { seconds } => seconds.to_bits(),
        };
        self.0.store(bits, Ordering::Release);
    }

    pub(super) fn unknown() -> Self {
        Self(AtomicU64::new(f64::NAN.to_bits()))
    }
}

/// Where a new track should land in the queue's internal `Vec`.
#[derive(Clone, Copy, Debug)]
pub(super) enum Placement {
    /// Push past the tail — used by `Queue::append`.
    Append,
    /// Insert at a caller-resolved position — used by `Queue::insert`
    /// after it looks up `after_id`.
    At(usize),
}

/// Current playback position and total duration in seconds, bundled
/// so the `should_arm_crossfade` signature does not put 3 consecutive
/// raw float parameters at the API boundary.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PlaybackTime {
    pub(crate) dur: f64,
    pub(crate) pos: f64,
}

/// Decide whether `Queue::tick` should arm the pre-end advance.
///
/// Returns `true` when:
/// - `crossfade > 0` (no pre-arm without crossfade — natural-EOF advance is
///   handled via [`PlayerEvent::ItemDidPlayToEnd`] instead), AND
/// - `time.pos` and `time.dur` are positive (track has meaningful position + duration), AND
/// - remaining playtime is below `crossfade` seconds, AND
/// - we haven't already armed for this track this play-through.
pub(crate) fn should_arm_crossfade(
    time: PlaybackTime,
    crossfade: f32,
    current_id: TrackId,
    armed_for: CrossfadeArm,
) -> bool {
    let PlaybackTime { pos, dur } = time;
    crossfade > 0.0
        && dur > 0.0
        && pos > 0.0
        && dur - pos <= f64::from(crossfade)
        && !armed_for.is_armed_for(current_id)
}

pub(super) fn extract_track_name(source: &TrackSource) -> String {
    let raw = match source {
        TrackSource::Uri(s) => s.as_str(),
        TrackSource::Config(cfg) => return name_from_src(cfg.source()),
    };
    name_from_raw(raw)
}

fn name_from_src(src: &ResourceSrc) -> String {
    match src {
        ResourceSrc::Url(url) => {
            let path = url.path();
            name_from_raw(path)
        }
        ResourceSrc::Path(p) => p.file_name().map_or_else(
            || "Unknown".to_string(),
            |n| n.to_string_lossy().into_owned(),
        ),
    }
}

fn name_from_raw(s: &str) -> String {
    s.rsplit('/')
        .find(|seg| !seg.is_empty())
        .unwrap_or("Unknown")
        .to_string()
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn atomic_track_id_disarmed_loads_disarmed() {
        let cell = AtomicTrackId::disarmed();
        assert_eq!(cell.load(), CrossfadeArm::Disarmed);
    }

    #[kithara::test]
    fn atomic_track_id_take_if_matches_only_disarms_matching_track() {
        let cell = AtomicTrackId::disarmed();
        cell.store(CrossfadeArm::armed(TrackId(7)));
        assert_eq!(
            cell.load(),
            CrossfadeArm::Armed {
                for_track: TrackId(7),
            }
        );
        assert!(!cell.take_if_matches(TrackId(8)));
        assert_eq!(
            cell.load(),
            CrossfadeArm::Armed {
                for_track: TrackId(7),
            }
        );
        assert!(cell.take_if_matches(TrackId(7)));
        assert_eq!(cell.load(), CrossfadeArm::Disarmed);
    }

    #[kithara::test]
    fn atomic_track_id_cas_arm_then_disarm() {
        let cell = AtomicTrackId::disarmed();
        assert!(cell.arm_if_disarmed(TrackId(3)));
        assert!(!cell.arm_if_disarmed(TrackId(4)));
        assert!(!cell.disarm_if_matches(TrackId(4)));
        assert!(cell.disarm_if_matches(TrackId(3)));
        assert_eq!(cell.load(), CrossfadeArm::Disarmed);
    }

    #[kithara::test]
    fn atomic_cached_position_unknown_loads_none() {
        let cell = AtomicCachedPosition::unknown();
        assert_eq!(Option::<f64>::from(cell.load()), None);
    }

    #[kithara::test]
    fn atomic_cached_position_round_trip_zero() {
        let cell = AtomicCachedPosition::unknown();
        cell.store(CachedPosition::known(0.0));
        assert_eq!(Option::<f64>::from(cell.load()), Some(0.0));
    }

    #[kithara::test]
    fn cached_position_known_nan_canonicalises_to_unknown() {
        assert!(matches!(
            CachedPosition::known(f64::NAN),
            CachedPosition::Unknown
        ));
    }

    #[kithara::test]
    fn crossfade_arm_is_armed_for_matches_track() {
        let arm = CrossfadeArm::armed(TrackId(2));
        assert!(arm.is_armed_for(TrackId(2)));
        assert!(!arm.is_armed_for(TrackId(3)));
    }

    #[kithara::test]
    fn select_phase_pending_carries_transition() {
        let phase = SelectPhase::Pending(PendingSelect {
            id: TrackId(5),
            transition: Transition::None,
        });
        match phase {
            SelectPhase::Pending(p) => {
                assert_eq!(p.id, TrackId(5));
                assert_eq!(p.transition, Transition::None);
            }
            SelectPhase::Idle => panic!("expected Pending"),
        }
    }
}

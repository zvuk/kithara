#![forbid(unsafe_code)]

use std::sync::atomic::{AtomicU64, Ordering};

use kithara_platform::time::Duration;
use kithara_test_utils::kithara;

const NO_DURATION: u64 = u64::MAX;

/// Decoder-reported chunk position used to advance the playhead.
///
/// This struct is the kithara-stream-local mirror of the fields
/// [`PlayheadWrite::advance`] needs from a decoder's per-chunk metadata.
/// It exists because `PcmMeta` lives in `kithara-decode` (which depends on
/// `kithara-stream`); a tiny mirror avoids the circular dep without forcing
/// decoders to fragment their existing meta type.
///
/// Decoder backends fill it from their own meta — see
/// `From<&PcmMeta> for ChunkPosition` in `kithara-decode`.
#[derive(Debug, Clone, Copy)]
pub struct ChunkPosition {
    /// Absolute byte offset of the chunk's source data when the
    /// decoder reports it (Apple `mStartOffset`, Android API 28+).
    pub source_byte_offset: Option<u64>,
    /// Decoder-reported wall-clock position **after** the chunk has
    /// been emitted (or, for a seek landing, the landed position).
    /// Authoritative — derived from the decoder's own frame counter
    /// inside its own arithmetic, so the playhead never recomputes
    /// `frames * 1e9 / sample_rate`. Always strictly greater than (or
    /// equal to, for seek landings) the chunk start.
    pub end_position_ns: u64,
    /// Absolute frame offset of the *first* frame in the chunk.
    pub frame_offset: u64,
    /// Number of frames the chunk covers.
    pub frames: u64,
    /// Source bytes the chunk decoded from (decoder ground truth).
    pub source_bytes: u64,
}

/// Source-agnostic playback playhead. The committed position is the single
/// coherent value readers need — a lone atomic, no torn read across fields.
#[derive(Debug)]
pub struct PlayheadState {
    position_ns: AtomicU64,
    total_duration_ns: AtomicU64,
    decoded_frontier_ns: AtomicU64,
}

/// Read-only view of the committed playhead.
pub trait PlayheadRead: Send + Sync {
    fn decoded_frontier(&self) -> Duration;
    fn duration(&self) -> Option<Duration>;
    fn position(&self) -> Duration;
}

/// Mutating view — the decode/produce path holds this. It is the ONLY mutator;
/// no à-la-carte field setters to desync.
pub trait PlayheadWrite: PlayheadRead {
    /// Advance to the end of a consumed chunk (caps at total duration).
    fn advance(&self, pos: &ChunkPosition);
    /// Pin after a seek (caps at total duration).
    fn land(&self, pos: &ChunkPosition);
    fn set_duration(&self, duration: Option<Duration>);
    fn set_decoded_frontier(&self, t: Duration);
    /// Partial-chunk fixup — raw store, no duration cap.
    fn set_position(&self, position: Duration);
}

impl PlayheadState {
    #[must_use]
    // ast-grep-ignore: style.prefer-default-derive
    pub fn new() -> Self {
        Self {
            position_ns: AtomicU64::new(0),
            total_duration_ns: AtomicU64::new(NO_DURATION),
            decoded_frontier_ns: AtomicU64::new(0),
        }
    }

    fn cap(&self) -> u64 {
        match self.total_duration_ns.load(Ordering::Acquire) {
            NO_DURATION => u64::MAX,
            d => d,
        }
    }

    fn write_ns_capped(&self, ns: u64) {
        self.position_ns
            .store(ns.min(self.cap()), Ordering::Release);
    }

    /// Capped playhead write that also fires the `committed_ns` USDT probe.
    /// The probe lives here because the produce path advances the playhead
    /// directly through this `PlayheadWrite` handle. The FLAC
    /// `swallow_detector` consumes this probe (`tests/src/swallow_detector.rs`).
    #[kithara::probe(committed_ns = pos.end_position_ns)]
    fn write_playhead(&self, pos: &ChunkPosition) {
        self.write_ns_capped(pos.end_position_ns);
    }
}

impl Default for PlayheadState {
    fn default() -> Self {
        Self::new()
    }
}

impl PlayheadRead for PlayheadState {
    fn decoded_frontier(&self) -> Duration {
        Duration::from_nanos(self.decoded_frontier_ns.load(Ordering::Relaxed))
    }

    fn duration(&self) -> Option<Duration> {
        match self.total_duration_ns.load(Ordering::Acquire) {
            NO_DURATION => None,
            d => Some(Duration::from_nanos(d)),
        }
    }

    fn position(&self) -> Duration {
        Duration::from_nanos(self.position_ns.load(Ordering::Acquire))
    }
}

impl PlayheadWrite for PlayheadState {
    fn advance(&self, pos: &ChunkPosition) {
        self.write_playhead(pos);
    }

    fn land(&self, pos: &ChunkPosition) {
        self.write_playhead(pos);
    }

    fn set_duration(&self, duration: Option<Duration>) {
        let raw = duration
            .and_then(|d| u64::try_from(d.as_nanos()).ok())
            .unwrap_or(NO_DURATION);
        self.total_duration_ns.store(raw, Ordering::Release);
    }

    fn set_decoded_frontier(&self, t: Duration) {
        let nanos = u64::try_from(t.as_nanos()).unwrap_or(u64::MAX);
        self.decoded_frontier_ns.fetch_max(nanos, Ordering::Relaxed);
    }

    fn set_position(&self, position: Duration) {
        let nanos = u64::try_from(position.as_nanos())
            .expect("BUG: position.as_nanos() fits in u64 for any realistic playback time");
        self.position_ns.store(nanos, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn position_caps_at_duration() {
        let s = PlayheadState::new();
        s.set_duration(Some(Duration::from_secs(5)));
        s.set_position(Duration::from_secs(9));
        // set_position is a raw store — no cap.
        assert_eq!(s.position(), Duration::from_secs(9));
    }

    #[kithara::test]
    fn advance_under_duration_reflects_end_then_caps() {
        let s = PlayheadState::new();
        s.set_duration(Some(Duration::from_secs(10)));

        let pos = ChunkPosition {
            frame_offset: 0,
            frames: 44100,
            end_position_ns: 1_000_000_000,
            source_bytes: 4096,
            source_byte_offset: None,
        };
        s.advance(&pos);
        assert_eq!(
            s.position(),
            Duration::from_nanos(1_000_000_000),
            "position must reflect end_position_ns when under duration"
        );

        let pos_past = ChunkPosition {
            frame_offset: 0,
            frames: 44100,
            end_position_ns: 15_000_000_000,
            source_bytes: 4096,
            source_byte_offset: None,
        };
        s.advance(&pos_past);
        assert_eq!(
            s.position(),
            Duration::from_secs(10),
            "position must be capped at total_duration"
        );
    }

    #[kithara::test]
    fn advance_caps_at_duration() {
        let s = PlayheadState::new();
        s.set_duration(Some(Duration::from_secs(5)));
        let pos = ChunkPosition {
            frame_offset: 0,
            frames: 44100,
            end_position_ns: 9_000_000_000,
            source_bytes: 0,
            source_byte_offset: None,
        };
        s.advance(&pos);
        assert_eq!(s.position(), Duration::from_secs(5));
    }

    #[kithara::test]
    fn land_caps_at_duration() {
        let s = PlayheadState::new();
        s.set_duration(Some(Duration::from_secs(5)));
        let pos = ChunkPosition {
            frame_offset: 0,
            frames: 0,
            end_position_ns: 9_000_000_000,
            source_bytes: 0,
            source_byte_offset: None,
        };
        s.land(&pos);
        assert_eq!(s.position(), Duration::from_secs(5));
    }

    #[kithara::test]
    fn no_duration_allows_any_position_via_advance() {
        let s = PlayheadState::new();
        // No duration set — cap is u64::MAX, advance goes through freely
        let pos = ChunkPosition {
            frame_offset: 0,
            frames: 44100,
            end_position_ns: 100_000_000_000,
            source_bytes: 0,
            source_byte_offset: None,
        };
        s.advance(&pos);
        assert_eq!(s.position(), Duration::from_nanos(100_000_000_000));
    }

    #[kithara::test]
    fn duration_none_round_trips() {
        let s = PlayheadState::new();
        assert_eq!(s.duration(), None);
        s.set_duration(Some(Duration::from_secs(10)));
        assert_eq!(s.duration(), Some(Duration::from_secs(10)));
        s.set_duration(None);
        assert_eq!(s.duration(), None);
    }

    #[kithara::test]
    fn decoded_frontier_is_monotonic() {
        let s = PlayheadState::new();
        assert_eq!(s.decoded_frontier(), Duration::ZERO);

        s.set_decoded_frontier(Duration::from_millis(900));
        s.set_decoded_frontier(Duration::from_millis(400));

        assert_eq!(s.decoded_frontier(), Duration::from_millis(900));
    }
}

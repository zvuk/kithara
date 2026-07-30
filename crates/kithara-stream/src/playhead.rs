#![forbid(unsafe_code)]

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use kithara_platform::time::Duration;
use kithara_test_utils::kithara;

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
    /// Share of the media already on disk, in parts per million. Stored as a
    /// dimensionless fraction rather than seconds because the download side
    /// learns it in bytes long before the duration settles; [`Self::cached`]
    /// projects it onto the timeline at read time using the duration that is
    /// current then.
    cached_ppm: AtomicU32,
    decoded_frontier_ns: AtomicU64,
    position_ns: AtomicU64,
    total_duration_ns: AtomicU64,
}

/// Read-only view of the committed playhead.
pub trait PlayheadRead: Send + Sync {
    /// Span of the timeline that is already on disk, measured from the start.
    ///
    /// This is what "available without more network" means to a host progress
    /// bar. `Duration::ZERO` while the duration or the source's total size is
    /// still unknown. Independent of [`Self::decoded_frontier`]: bytes can be
    /// cached long before they are decoded, and a decoder can run ahead of what
    /// the download side has reported.
    fn cached(&self) -> Duration;
    fn decoded_frontier(&self) -> Duration;
    fn duration(&self) -> Option<Duration>;
    fn position(&self) -> Duration;
}

/// Mutating view — the decode/produce path holds this. It is the ONLY mutator;
/// no à-la-carte field setters to desync.
pub trait PlayheadWrite: PlayheadRead {
    /// Advance to the end of a consumed chunk (caps at total duration).
    fn advance(&self, pos: &ChunkPosition);
    /// Partial playback-progress write, capped at total duration.
    fn advance_partial(&self, position: Duration);
    /// Pin after a seek to decoder truth, even when duration metadata is stale.
    fn land(&self, pos: &ChunkPosition);
    /// Publish how much of the source is on disk, as a byte prefix measured
    /// from offset zero. The source's download side is the only writer — it is
    /// the canonical owner of what the asset store holds.
    fn set_cached(&self, cached_bytes: u64, total_bytes: u64);
    fn set_decoded_frontier(&self, t: Duration);
    fn set_duration(&self, duration: Option<Duration>);
    /// Absolute seek/reset write, not capped by duration metadata.
    fn set_position(&self, position: Duration);
}

impl PlayheadState {
    /// Fixed-point scale for `cached_ppm`: the value meaning "fully cached".
    const CACHED_FULL_PPM: u32 = 1_000_000;
    /// Sentinel stored in `total_duration_ns` for "duration unknown".
    const NO_DURATION: u64 = u64::MAX;

    #[must_use]
    // ast-grep-ignore: style.prefer-default-derive
    pub fn new() -> Self {
        Self {
            position_ns: AtomicU64::new(0),
            total_duration_ns: AtomicU64::new(Self::NO_DURATION),
            decoded_frontier_ns: AtomicU64::new(0),
            cached_ppm: AtomicU32::new(0),
        }
    }

    fn cap(&self) -> u64 {
        match self.total_duration_ns.load(Ordering::Acquire) {
            Self::NO_DURATION => u64::MAX,
            d => d,
        }
    }

    fn write_ns(&self, ns: u64) {
        self.position_ns.store(ns, Ordering::Release);
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
    fn cached(&self) -> Duration {
        let ppm = self.cached_ppm.load(Ordering::Relaxed);
        self.duration()
            .map(|duration| duration.mul_f64(f64::from(ppm) / f64::from(Self::CACHED_FULL_PPM)))
            .unwrap_or_default()
    }

    fn decoded_frontier(&self) -> Duration {
        Duration::from_nanos(self.decoded_frontier_ns.load(Ordering::Relaxed))
    }

    fn duration(&self) -> Option<Duration> {
        match self.total_duration_ns.load(Ordering::Acquire) {
            Self::NO_DURATION => None,
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

    fn advance_partial(&self, position: Duration) {
        let nanos = u64::try_from(position.as_nanos()).unwrap_or(u64::MAX);
        self.write_ns_capped(nanos);
    }

    fn land(&self, pos: &ChunkPosition) {
        self.write_ns(pos.end_position_ns);
    }

    fn set_cached(&self, cached_bytes: u64, total_bytes: u64) {
        let Some(total) = (total_bytes > 0).then(|| u128::from(total_bytes)) else {
            return;
        };
        let cached = u128::from(cached_bytes.min(total_bytes));
        let ppm = cached * u128::from(Self::CACHED_FULL_PPM) / total;
        self.cached_ppm.store(
            u32::try_from(ppm).unwrap_or(Self::CACHED_FULL_PPM),
            Ordering::Relaxed,
        );
    }

    fn set_decoded_frontier(&self, t: Duration) {
        let nanos = u64::try_from(t.as_nanos()).unwrap_or(u64::MAX);
        self.decoded_frontier_ns.fetch_max(nanos, Ordering::Relaxed);
    }

    fn set_duration(&self, duration: Option<Duration>) {
        let raw = duration
            .and_then(|d| u64::try_from(d.as_nanos()).ok())
            .unwrap_or(Self::NO_DURATION);
        self.total_duration_ns.store(raw, Ordering::Release);
    }

    fn set_position(&self, position: Duration) {
        let nanos = u64::try_from(position.as_nanos()).unwrap_or(u64::MAX);
        self.write_ns(nanos);
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn partial_advance_caps_at_duration() {
        let s = PlayheadState::new();
        s.set_duration(Some(Duration::from_secs(5)));
        s.advance_partial(Duration::from_secs(9));
        assert_eq!(s.position(), Duration::from_secs(5));
    }

    #[kithara::test]
    fn set_position_allows_seek_landing_past_stale_duration() {
        let s = PlayheadState::new();
        s.set_duration(Some(Duration::from_secs(5)));
        s.set_position(Duration::from_secs(9));
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
    fn land_allows_seek_landing_past_stale_duration() {
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
        assert_eq!(s.position(), Duration::from_secs(9));
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

    #[kithara::test]
    fn cached_projects_the_byte_prefix_onto_the_timeline() {
        let s = PlayheadState::new();
        s.set_duration(Some(Duration::from_secs(200)));
        s.set_cached(250, 1000);
        assert_eq!(s.cached(), Duration::from_secs(50));
    }

    #[kithara::test]
    fn cached_covers_the_whole_track_once_every_byte_landed() {
        let s = PlayheadState::new();
        s.set_duration(Some(Duration::from_secs(187)));
        s.set_cached(2_994_349, 2_994_349);
        assert_eq!(s.cached(), Duration::from_secs(187));
    }

    /// The download side reports bytes long before an MP3's duration settles,
    /// so the fraction has to survive until a duration exists to project it on.
    #[kithara::test]
    fn cached_survives_a_duration_that_settles_after_the_bytes_landed() {
        let s = PlayheadState::new();
        s.set_cached(1000, 1000);
        assert_eq!(s.cached(), Duration::ZERO);

        s.set_duration(Some(Duration::from_secs(120)));
        assert_eq!(s.cached(), Duration::from_secs(120));
    }

    #[kithara::test]
    fn cached_ignores_an_unknown_total_size() {
        let s = PlayheadState::new();
        s.set_duration(Some(Duration::from_secs(100)));
        s.set_cached(500, 1000);
        s.set_cached(900, 0);
        assert_eq!(s.cached(), Duration::from_secs(50));
    }
}

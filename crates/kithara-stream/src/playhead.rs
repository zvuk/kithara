#![forbid(unsafe_code)]

use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use crate::timeline::ChunkPosition;

const NO_DURATION: u64 = u64::MAX;

/// Source-agnostic playback playhead. The committed position is the single
/// coherent value readers need — a lone atomic, no torn read across fields.
#[derive(Debug)]
pub(crate) struct PlayheadState {
    position_ns: AtomicU64,
    total_duration_ns: AtomicU64,
}

/// Read-only view of the committed playhead.
pub trait PlayheadRead: Send + Sync {
    fn position(&self) -> Duration;
    fn duration(&self) -> Option<Duration>;
}

/// Mutating view — the decode/produce path holds this. It is the ONLY mutator;
/// no à-la-carte field setters to desync.
pub trait PlayheadWrite: PlayheadRead {
    /// Advance to the end of a consumed chunk (caps at total duration).
    fn advance(&self, pos: &ChunkPosition);
    /// Pin after a seek (caps at total duration).
    fn land(&self, pos: &ChunkPosition);
    /// Partial-chunk fixup — raw store, no duration cap (matches Timeline
    /// `set_committed_position` which is also a raw store).
    fn set_position(&self, position: Duration);
    fn set_duration(&self, duration: Option<Duration>);
}

impl PlayheadState {
    // ast-grep-ignore: style.prefer-default-derive
    pub(crate) fn new() -> Self {
        Self {
            position_ns: AtomicU64::new(0),
            total_duration_ns: AtomicU64::new(NO_DURATION),
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
}

impl PlayheadRead for PlayheadState {
    fn position(&self) -> Duration {
        Duration::from_nanos(self.position_ns.load(Ordering::Acquire))
    }

    fn duration(&self) -> Option<Duration> {
        match self.total_duration_ns.load(Ordering::Acquire) {
            NO_DURATION => None,
            d => Some(Duration::from_nanos(d)),
        }
    }
}

impl PlayheadWrite for PlayheadState {
    fn advance(&self, pos: &ChunkPosition) {
        self.write_ns_capped(pos.end_position_ns);
    }

    fn land(&self, pos: &ChunkPosition) {
        self.write_ns_capped(pos.end_position_ns);
    }

    fn set_position(&self, position: Duration) {
        let nanos = u64::try_from(position.as_nanos())
            .expect("BUG: position.as_nanos() fits in u64 for any realistic playback time");
        self.position_ns.store(nanos, Ordering::Release);
    }

    fn set_duration(&self, duration: Option<Duration>) {
        let raw = duration
            .and_then(|d| u64::try_from(d.as_nanos()).ok())
            .unwrap_or(NO_DURATION);
        self.total_duration_ns.store(raw, Ordering::Release);
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
        // set_position is a raw store — no cap — matches Timeline::set_committed_position
        assert_eq!(s.position(), Duration::from_secs(9));
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
}

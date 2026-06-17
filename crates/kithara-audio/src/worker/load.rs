use std::sync::atomic::Ordering;

use kithara_platform::time::Duration;
use portable_atomic::AtomicF32;

/// EWMA weight for per-chunk samples (≈ last ~10 chunks dominate).
const LOAD_ALPHA: f32 = 0.2;

fn to_f64(x: usize) -> f64 {
    num_traits::cast(x).unwrap_or_default()
}

fn to_f32(x: f64) -> f32 {
    num_traits::cast(x).unwrap_or_default()
}

/// Blend `sample` into `prev`; seeds from the first sample so the meter
/// settles immediately instead of ramping up from zero.
fn ewma(prev: f32, sample: f32) -> f32 {
    if prev <= 0.0 {
        sample
    } else {
        prev * (1.0 - LOAD_ALPHA) + sample * LOAD_ALPHA
    }
}

/// Live cost of the audio worker's decode+effects pipeline.
/// The atomics double as the EWMA state. There is a single writer (the
/// worker thread; every track's node ticks on it). Values stay at `0.0` until
/// the first chunk, so `realtime == 0` means "no measurement yet" (paused
/// / not started).
#[derive(Debug)]
pub struct EngineLoad {
    /// Produced audio-seconds per CPU-second (`>1` = faster than realtime).
    realtime: AtomicF32,
    /// Fraction of realtime spent producing (`busy / audio`, `0.05` = 5%).
    load: AtomicF32,
    /// Wall time spent per produced chunk, in milliseconds.
    ms: AtomicF32,
}

impl Default for EngineLoad {
    fn default() -> Self {
        Self {
            realtime: AtomicF32::new(0.0),
            load: AtomicF32::new(0.0),
            ms: AtomicF32::new(0.0),
        }
    }
}

impl EngineLoad {
    /// Fold one produced chunk's cost into the EWMA (worker thread).
    ///
    /// `busy` is the wall time the tick spent producing `frames` output
    /// frames at `sample_rate`. Skips empty chunks.
    pub(crate) fn record(&self, busy: Duration, frames: usize, sample_rate: u32) {
        if frames == 0 {
            return;
        }
        let audio_secs = to_f64(frames) / f64::from(sample_rate.max(1));
        if audio_secs <= 0.0 {
            return;
        }
        let busy_secs = busy.as_secs_f64();
        let load = ewma(
            self.load.load(Ordering::Relaxed),
            to_f32(busy_secs / audio_secs),
        );
        let ms = ewma(self.ms.load(Ordering::Relaxed), to_f32(busy_secs * 1000.0));
        let realtime = if load > 0.0 { 1.0 / load } else { 0.0 };
        self.realtime.store(realtime, Ordering::Relaxed);
        self.load.store(load, Ordering::Relaxed);
        self.ms.store(ms, Ordering::Relaxed);
    }

    /// Read a copyable snapshot (UI thread).
    #[must_use]
    pub fn snapshot(&self) -> EngineLoadSnapshot {
        EngineLoadSnapshot {
            realtime: self.realtime.load(Ordering::Relaxed),
            load: self.load.load(Ordering::Relaxed),
            ms: self.ms.load(Ordering::Relaxed),
        }
    }
}

/// Copyable view of [`EngineLoad`] for cross-thread reads / UI rendering.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
#[non_exhaustive]
pub struct EngineLoadSnapshot {
    /// Produced audio-seconds per CPU-second (`>1` = faster than realtime).
    pub realtime: f32,
    /// Fraction of realtime spent producing (`0.05` = 5% load).
    pub load: f32,
    /// Wall time per produced chunk, in milliseconds.
    pub ms: f32,
}

impl EngineLoadSnapshot {
    /// `true` once the worker has produced at least one measurement.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.realtime > 0.0
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn record_populates_snapshot_and_skips_empty() {
        let meter = EngineLoad::default();
        assert!(!meter.snapshot().is_active(), "idle before any record");

        // Empty chunk records nothing.
        meter.record(Duration::from_millis(5), 0, 44_100);
        assert!(!meter.snapshot().is_active(), "empty chunk records nothing");

        // 5 ms of CPU to produce 100 ms of audio → load ≈ 0.05, realtime ≈ 20x.
        for _ in 0..8 {
            meter.record(Duration::from_millis(5), 4_410, 44_100);
        }
        let s = meter.snapshot();
        assert!(s.is_active(), "active after producing: {s:?}");
        assert!(s.realtime > 1.0, "faster than realtime: {s:?}");
        assert!(
            s.load > 0.0 && s.load < 1.0,
            "load fraction in (0,1): {s:?}"
        );
        assert!(s.ms > 0.0, "ms populated: {s:?}");
    }
}

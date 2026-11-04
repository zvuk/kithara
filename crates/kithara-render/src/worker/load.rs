use kithara_platform::{atomic::RelaxedAtomicF32, time::Duration};

use crate::consts;

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
        sample.mul_add(consts::LOAD_ALPHA, prev * (1.0 - consts::LOAD_ALPHA))
    }
}

/// Live cost of the audio worker's decode+effects pipeline.
/// The atomics double as the EWMA state. There is a single writer (the
/// worker thread; every track's node ticks on it). Values stay at `0.0` until
/// the first chunk, so `realtime == 0` means "no measurement yet" (paused
/// / not started).
#[derive(Debug)]
pub struct EngineLoad {
    /// Fraction of realtime spent producing (`busy / audio`, `0.05` = 5%).
    load: RelaxedAtomicF32,
    /// Wall time spent per produced chunk, in milliseconds.
    ms: RelaxedAtomicF32,
    /// Produced audio-seconds per CPU-second (`>1` = faster than realtime).
    realtime: RelaxedAtomicF32,
}

impl Default for EngineLoad {
    fn default() -> Self {
        Self {
            realtime: RelaxedAtomicF32::new(0.0),
            load: RelaxedAtomicF32::new(0.0),
            ms: RelaxedAtomicF32::new(0.0),
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
        let load = ewma(self.load.load(), to_f32(busy_secs / audio_secs));
        let ms = ewma(self.ms.load(), to_f32(busy_secs * consts::MS_PER_SEC));
        let realtime = if load > 0.0 { 1.0 / load } else { 0.0 };
        self.realtime.store(realtime);
        self.load.store(load);
        self.ms.store(ms);
    }

    /// Read a copyable snapshot (UI thread).
    #[must_use]
    pub fn snapshot(&self) -> EngineLoadSnapshot {
        EngineLoadSnapshot {
            realtime: self.realtime.load(),
            load: self.load.load(),
            ms: self.ms.load(),
        }
    }
}

/// Copyable view of [`EngineLoad`] for cross-thread reads / UI rendering.
#[derive(Debug, Clone, Copy, Default, PartialEq, fieldwork::Fieldwork)]
#[non_exhaustive]
#[fieldwork(get)]
pub struct EngineLoadSnapshot {
    /// Fraction of realtime spent producing (`0.05` = 5% load).
    load: f32,
    /// Wall time per produced chunk, in milliseconds.
    ms: f32,
    /// Produced audio-seconds per CPU-second (`>1` = faster than realtime).
    realtime: f32,
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

    const RATE: u32 = 48_000;
    const QUANTUM: usize = 128;

    fn busy_at(load: f64, frames: usize) -> Duration {
        Duration::from_secs_f64(to_f64(frames) / f64::from(RATE) * load)
    }

    #[kithara::test]
    fn record_populates_snapshot_and_skips_empty() {
        let meter = EngineLoad::default();
        assert!(!meter.snapshot().is_active(), "idle before any record");

        meter.record(Duration::from_millis(5), 0, 44_100);
        assert!(!meter.snapshot().is_active(), "empty chunk records nothing");

        // 5 ms of CPU to produce 100 ms of audio → load ≈ 0.05, realtime ≈ 20x.
        for _ in 0..8 {
            meter.record(Duration::from_millis(5), 4_410, 44_100);
        }
        let s = meter.snapshot();
        assert!(s.is_active(), "active after producing: {s:?}");
        assert!(s.realtime() > 1.0, "faster than realtime: {s:?}");
        assert!(
            s.load() > 0.0 && s.load() < 1.0,
            "load fraction in (0,1): {s:?}"
        );
        assert!(s.ms() > 0.0, "ms populated: {s:?}");
    }

    #[kithara::test]
    fn the_same_cost_reads_the_same_however_the_audio_is_chunked() {
        let chunkings = [
            vec![2_400],
            [vec![QUANTUM; 18], vec![96]].concat(),
            [vec![QUANTUM; 18], vec![95, 1]].concat(),
        ];
        let readings: Vec<f32> = chunkings
            .iter()
            .map(|chunks| {
                let meter = EngineLoad::default();
                meter.record(Duration::from_millis(10), 48_000, RATE);
                for &frames in chunks {
                    meter.record(busy_at(0.05, frames), frames, RATE);
                }
                meter.snapshot().load()
            })
            .collect();
        let low = readings.iter().copied().fold(f32::INFINITY, f32::min);
        let high = readings.iter().copied().fold(0.0, f32::max);
        assert!(
            (high - low) / high <= 1e-3,
            "chunkings agree within 0.1%: {readings:?}"
        );
        assert!(
            readings.iter().all(|&load| load > 0.01 && load < 0.05),
            "each reading lies between 1% and 5%: {readings:?}"
        );
    }

    #[kithara::test]
    fn a_tiny_chunk_moves_a_settled_reading_by_at_most_its_cost_over_the_time_constant() {
        let settle_secs = (consts::LOAD_TIME_CONSTANT * 10).as_secs_f64();
        let quanta: usize =
            num_traits::cast((settle_secs * f64::from(RATE) / to_f64(QUANTUM)).ceil())
                .expect("settling quanta fit usize");
        let meter = EngineLoad::default();
        for _ in 0..quanta {
            meter.record(busy_at(0.01, QUANTUM), QUANTUM, RATE);
        }
        let settled = meter.snapshot().load();
        let tiny = Duration::from_micros(8);
        meter.record(tiny, 1, RATE);
        let rise = f64::from(meter.snapshot().load() - settled);
        let bound = tiny.as_secs_f64() / consts::LOAD_TIME_CONSTANT.as_secs_f64();
        assert!(
            rise <= bound,
            "one-frame chunk raised {settled} by {rise}, over {bound}"
        );
    }
}

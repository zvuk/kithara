use kithara_platform::{atomic::RelaxedAtomicF32, time::Duration};

use crate::consts;

fn to_f64(x: usize) -> f64 {
    num_traits::cast(x).unwrap_or_default()
}

fn to_f32(x: f64) -> f32 {
    num_traits::cast(x).unwrap_or_default()
}

/// Live cost of producing one deck's audio: busy time over audio time.
///
/// Each chunk's busy time is spread evenly over its audio, and older audio
/// decays exponentially with audio time at one fixed time constant. The same
/// audio produced at the same cost reads the same however it is chunked, and
/// one short chunk moves a settled reading by at most its busy time over the
/// time constant. There is a single writer (the worker thread; every track's
/// node ticks on it). The load stays `0.0` until a chunk that took time to
/// produce, so `realtime == 0` means "no measurement yet" (paused / not
/// started).
#[derive(Debug)]
pub struct EngineLoad {
    /// Decayed audio seconds measured so far (writer only); `0.0` until the
    /// first chunk.
    audio: RelaxedAtomicF32,
    /// Decayed busy seconds over `audio` (`0.05` = 5%).
    load: RelaxedAtomicF32,
}

impl Default for EngineLoad {
    fn default() -> Self {
        Self {
            audio: RelaxedAtomicF32::new(0.0),
            load: RelaxedAtomicF32::new(0.0),
        }
    }
}

impl EngineLoad {
    /// Fold one produced chunk's cost into the reading (worker thread).
    ///
    /// `busy` is the wall time the tick spent producing `frames` output
    /// frames at `sample_rate`. Skips empty chunks.
    pub(crate) fn record(&self, busy: Duration, frames: usize, sample_rate: u32) {
        if frames == 0 {
            return;
        }
        let audio_secs = to_f64(frames) / f64::from(sample_rate.max(1));
        let time_constant = consts::LOAD_TIME_CONSTANT.as_secs_f64();
        // `1 - exp` would lose the weight of a chunk far shorter than the constant.
        let forgotten = -(-audio_secs / time_constant).exp_m1();
        let kept = f64::from(self.audio.load()) * (1.0 - forgotten);
        let fresh = time_constant * forgotten;
        let audio = kept + fresh;
        let spent =
            f64::from(self.load.load()).mul_add(kept, busy.as_secs_f64() / audio_secs * fresh);
        self.audio.store(to_f32(audio));
        self.load.store(to_f32(spent / audio));
    }

    /// Read a copyable snapshot (UI thread).
    #[must_use]
    pub fn snapshot(&self) -> EngineLoadSnapshot {
        let load = self.load.load();
        EngineLoadSnapshot {
            load,
            realtime: if load > 0.0 { load.recip() } else { 0.0 },
        }
    }
}

/// Copyable view of [`EngineLoad`] for cross-thread reads / UI rendering.
#[derive(Debug, Clone, Copy, Default, PartialEq, fieldwork::Fieldwork)]
#[non_exhaustive]
#[fieldwork(get)]
pub struct EngineLoadSnapshot {
    /// Busy time over audio time (`0.05` = 5% load).
    load: f32,
    /// Produced audio-seconds per CPU-second, the reciprocal of `load`
    /// (`>1` = faster than realtime); `0.0` until measured.
    realtime: f32,
}

impl EngineLoadSnapshot {
    /// `true` once the worker has measured a chunk that took time to produce.
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
        assert_eq!(
            meter.snapshot().load(),
            0.0,
            "empty chunk leaves load at zero"
        );

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
        assert_eq!(
            s.realtime(),
            s.load().recip(),
            "realtime is 1 / load: {s:?}"
        );
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

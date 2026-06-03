use std::sync::Arc;
use std::sync::atomic::Ordering;

use kithara_bufpool::PcmPool;
use kithara_decode::{PcmChunk, PcmMeta, PcmSpec};
use portable_atomic::AtomicF32;
use tracing::warn;

use super::stretch_backend::StretchBackend;
use super::stretch_factory::build_backend;
use super::stretch_kind::StretchBackendKind;
use crate::traits::AudioEffect;

/// Floor for the shared playback speed before inverting to a stretch
/// factor. Higher than the resampler's `0.01` floor: at `speed = 0.05` the
/// stretch is already 20x, beyond which time-stretch quality collapses, so
/// there is no point clamping lower.
const MIN_SPEED: f32 = 0.05;
/// Re-apply the stretch ratio to the backend only when it moves this much.
const RATIO_EPS: f64 = 1e-4;

/// Pre-resampler time-stretch slot: drives one [`StretchBackend`] (chosen
/// once at chain build) and owns all chunk / pool / timeline plumbing.
///
/// Reads the shared playback `speed` (>1 faster) from `tempo_ratio` and
/// hands the backend the inverse stretch factor; pitch is held at `1.0`
/// (tempo mode preserves pitch by definition). Output frame count differs
/// from input, so each emitted chunk's `frames` is recomputed while the
/// decoder's `timestamp` / `end_timestamp` / `spec` are preserved verbatim
/// — the playhead stays in source-track time. See the resampler for the
/// same meta recipe and `crates/kithara-audio/README.md` for the contract.
pub struct TimeStretchProcessor {
    backend: Box<dyn StretchBackend>,
    kind: StretchBackendKind,
    tempo_ratio: Arc<AtomicF32>,
    pool: PcmPool,
    spec: PcmSpec,
    /// Last stretch factor pushed to the backend; avoids redundant updates.
    applied_stretch: f64,
    /// Most recent input meta, carried onto each output chunk.
    last_input_meta: Option<PcmMeta>,
    /// Interleaved output scratch, reused across calls (alloc-free steady state).
    scratch: Vec<f32>,
}

impl TimeStretchProcessor {
    /// Build the slot for `kind` at the source `spec`, driven by the shared
    /// `tempo_ratio` playback-speed atomic.
    pub fn new(
        kind: StretchBackendKind,
        spec: PcmSpec,
        tempo_ratio: Arc<AtomicF32>,
        pool: PcmPool,
    ) -> Self {
        let mut backend = build_backend(kind, spec);
        if let Err(e) = backend.set_pitch(1.0) {
            warn!(error = %e, "time-stretch set_pitch(1.0) failed");
        }
        let mut me = Self {
            backend,
            kind,
            tempo_ratio,
            pool,
            spec,
            applied_stretch: f64::NAN,
            last_input_meta: None,
            scratch: Vec::new(),
        };
        me.sync_ratio();
        me
    }

    /// Pull the shared playback speed and update the backend stretch factor
    /// (`1 / speed`) when it has moved.
    fn sync_ratio(&mut self) {
        let speed = self.tempo_ratio.load(Ordering::Relaxed).max(MIN_SPEED);
        let stretch = 1.0 / f64::from(speed);
        // `NaN` is the "never applied" sentinel; the diff test alone would
        // skip it (every comparison with NaN is false).
        if self.applied_stretch.is_nan() || (stretch - self.applied_stretch).abs() > RATIO_EPS {
            match self.backend.set_ratio(stretch) {
                Ok(()) => self.applied_stretch = stretch,
                Err(e) => warn!(error = %e, "time-stretch set_ratio failed"),
            }
        }
    }

    /// Assemble an output chunk from `scratch`, preserving decoder timing.
    fn emit(&mut self) -> Option<PcmChunk> {
        let total = self.scratch.len();
        if total == 0 {
            return None;
        }
        let channels = usize::from(self.spec.channels.max(1));
        let mut meta = self.last_input_meta.unwrap_or_default();
        meta.frames = u32::try_from(total / channels).unwrap_or(u32::MAX);
        let mut pcm = self.pool.get();
        if pcm.ensure_len(total).is_err() {
            warn!("PCM pool budget exhausted during time-stretch");
            return None;
        }
        pcm[..].copy_from_slice(&self.scratch);
        Some(PcmChunk::new(meta, pcm))
    }
}

impl AudioEffect for TimeStretchProcessor {
    fn flush(&mut self) -> Option<PcmChunk> {
        self.scratch.clear();
        if let Err(e) = self.backend.flush(&mut self.scratch) {
            warn!(error = %e, "time-stretch backend flush failed");
            return None;
        }
        self.emit()
    }

    fn process(&mut self, chunk: PcmChunk) -> Option<PcmChunk> {
        if chunk.spec() != self.spec {
            self.spec = chunk.spec();
            self.backend = build_backend(self.kind, self.spec);
            if let Err(e) = self.backend.set_pitch(1.0) {
                warn!(error = %e, "time-stretch set_pitch(1.0) failed");
            }
            self.applied_stretch = f64::NAN;
        }
        self.sync_ratio();
        self.last_input_meta = Some(chunk.meta);

        let needed = self.backend.max_output_samples(chunk.frames());
        if self.scratch.capacity() < needed {
            self.scratch.reserve(needed - self.scratch.len());
        }
        self.scratch.clear();
        if let Err(e) = self.backend.process(chunk.samples(), &mut self.scratch) {
            warn!(error = %e, "time-stretch backend process failed; dropping chunk");
            return None;
        }
        self.emit()
    }

    fn reset(&mut self) {
        self.backend.reset();
        self.scratch.clear();
        self.last_input_meta = None;
        self.applied_stretch = f64::NAN;
    }
}

#[cfg(test)]
mod tests {
    use kithara_bufpool::PcmPool;
    use kithara_decode::{PcmMeta, PcmSpec};
    use kithara_test_utils::kithara;
    use realfft::RealFftPlanner;

    use super::*;

    const SR: u32 = 44_100;
    const F0: f64 = 440.0;
    const CH: u16 = 2;
    /// FFT length for the pitch (dominant-frequency) check.
    const N: usize = 1 << 14;

    fn f32_of(x: f64) -> f32 {
        num_traits::cast(x).unwrap_or_default()
    }

    fn f64_of(x: usize) -> f64 {
        num_traits::cast(x).unwrap_or_default()
    }

    /// Interleaved stereo sine at `F0`, phase-accumulated to avoid drift.
    fn sine(frames: usize) -> Vec<f32> {
        let inc = std::f64::consts::TAU * F0 / f64::from(SR);
        let mut phase = 0.0_f64;
        let mut out = Vec::with_capacity(frames * usize::from(CH));
        for _ in 0..frames {
            let s = f32_of(0.5 * phase.sin());
            out.push(s);
            out.push(s);
            phase += inc;
        }
        out
    }

    fn chunk(samples: &[f32]) -> PcmChunk {
        let frames = samples.len() / usize::from(CH);
        PcmChunk::new(
            PcmMeta {
                spec: PcmSpec {
                    channels: CH,
                    sample_rate: SR,
                },
                frames: u32::try_from(frames).unwrap_or(0),
                timestamp: std::time::Duration::ZERO,
                ..Default::default()
            },
            PcmPool::default().attach(samples.to_vec()),
        )
    }

    /// Index of the strongest spectral bin (skipping DC) of a mono window
    /// taken from the middle of `mono`.
    fn dominant_bin(mono: &[f32]) -> usize {
        let start = (mono.len().saturating_sub(N)) / 2;
        let seg = &mono[start..start + N];
        let mut planner = RealFftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(N);
        let mut input = fft.make_input_vec();
        input.copy_from_slice(seg);
        let mut spectrum = fft.make_output_vec();
        fft.process(&mut input, &mut spectrum).unwrap();
        spectrum
            .iter()
            .enumerate()
            .skip(1)
            .max_by(|a, b| a.1.norm().total_cmp(&b.1.norm()))
            .map_or(0, |(i, _)| i)
    }

    fn expected_bin(freq: f64) -> usize {
        num_traits::cast((freq * f64_of(N) / f64::from(SR)).round()).unwrap_or(0)
    }

    fn run(kind: StretchBackendKind, speed: f32, in_frames: usize) -> Vec<f32> {
        let input = sine(in_frames);
        let tempo = Arc::new(AtomicF32::new(speed));
        let mut fx = TimeStretchProcessor::new(
            kind,
            PcmSpec {
                channels: CH,
                sample_rate: SR,
            },
            Arc::clone(&tempo),
            PcmPool::default().clone(),
        );
        let mut out: Vec<f32> = Vec::new();
        let block = 4096 * usize::from(CH);
        for data in input.chunks(block) {
            if let Some(c) = fx.process(chunk(data)) {
                assert_eq!(c.spec().sample_rate, SR, "stretch preserves sample rate");
                assert_eq!(c.spec().channels, CH);
                out.extend_from_slice(c.samples());
            }
        }
        while let Some(c) = fx.flush() {
            out.extend_from_slice(c.samples());
        }
        out
    }

    /// The C++ backends emit fixed-length output that carries leading
    /// latency-fill (and bungee also drops its tail), nudging the measured
    /// duration off an exact 2x on a short clip. The pure-Rust `Timestretch`
    /// emits variable-length output and lands tight, so only it is held to
    /// the strict band.
    fn has_latency_fill(_kind: StretchBackendKind) -> bool {
        #[cfg(all(not(target_arch = "wasm32"), feature = "stretch-signalsmith"))]
        if _kind == StretchBackendKind::Signalsmith {
            return true;
        }
        #[cfg(all(not(target_arch = "wasm32"), feature = "stretch-bungee"))]
        if _kind == StretchBackendKind::Bungee {
            return true;
        }
        false
    }

    /// Half playback speed -> stretch 2.0 -> ~double duration, pitch held.
    /// Shared across every compiled-in backend.
    fn assert_half_speed_contract(kind: StretchBackendKind) {
        let channels = usize::from(CH);
        let in_frames = usize::try_from(SR).unwrap() * 2; // 2 s
        let out = run(kind, 0.5, in_frames);
        let out_frames = out.len() / channels;

        // `Timestretch` (variable output) is held tight (±5%) so pitch leaking
        // into duration would fail here; the C++ backends carry latency-fill,
        // so they get a ±10% band — their pitch is still checked tightly below.
        if has_latency_fill(kind) {
            assert!(
                out_frames * 10 >= in_frames * 18 && out_frames * 10 <= in_frames * 22,
                "{kind:?}: expected ~2x duration, got {out_frames} from {in_frames}"
            );
        } else {
            assert!(
                out_frames * 100 >= in_frames * 190 && out_frames * 100 <= in_frames * 210,
                "{kind:?}: expected ~2x duration (±5%), got {out_frames} from {in_frames}"
            );
        }

        // Pitch preserved: dominant bin still at F0 (the load-bearing check —
        // a resampler-in-disguise would shift it).
        let mono: Vec<f32> = out.iter().step_by(channels).copied().collect();
        assert!(
            mono.len() >= N,
            "{kind:?}: not enough output for the FFT window"
        );
        let peak = dominant_bin(&mono);
        let want = expected_bin(F0);
        assert!(
            peak.abs_diff(want) <= 3,
            "{kind:?}: pitch moved under time-stretch: peak bin {peak}, expected {want}"
        );
    }

    fn assert_unity_contract(kind: StretchBackendKind) {
        let channels = usize::from(CH);
        let in_frames = usize::try_from(SR).unwrap() * 2;
        let out = run(kind, 1.0, in_frames);
        let out_frames = out.len() / channels;
        assert!(
            out_frames * 10 >= in_frames * 9 && out_frames * 10 <= in_frames * 12,
            "{kind:?}: expected ~1x duration, got {out_frames} from {in_frames}"
        );
        let mono: Vec<f32> = out.iter().step_by(channels).copied().collect();
        let peak = dominant_bin(&mono);
        assert!(
            peak.abs_diff(expected_bin(F0)) <= 3,
            "{kind:?}: pitch moved at unity speed"
        );
    }

    #[kithara::test]
    fn half_speed_doubles_duration_and_keeps_pitch() {
        assert_half_speed_contract(StretchBackendKind::Timestretch);
    }

    #[kithara::test]
    fn unity_speed_keeps_duration_and_pitch() {
        assert_unity_contract(StretchBackendKind::Timestretch);
    }

    #[cfg(all(not(target_arch = "wasm32"), feature = "stretch-signalsmith"))]
    #[kithara::test]
    fn signalsmith_half_speed_and_unity_contracts() {
        assert_half_speed_contract(StretchBackendKind::Signalsmith);
        assert_unity_contract(StretchBackendKind::Signalsmith);
    }

    #[cfg(all(not(target_arch = "wasm32"), feature = "stretch-bungee"))]
    #[kithara::test]
    fn bungee_half_speed_and_unity_contracts() {
        assert_half_speed_contract(StretchBackendKind::Bungee);
        assert_unity_contract(StretchBackendKind::Bungee);
    }

    #[kithara::test]
    fn output_meta_preserves_decoder_timeline() {
        let channels = usize::from(CH);
        let tempo = Arc::new(AtomicF32::new(0.5));
        let mut fx = TimeStretchProcessor::new(
            StretchBackendKind::Timestretch,
            PcmSpec {
                channels: CH,
                sample_rate: SR,
            },
            Arc::clone(&tempo),
            PcmPool::default().clone(),
        );
        let cf = 1024usize;
        let block = sine(cf);
        let mut fed_ends = std::collections::HashSet::new();
        let mut emitted = Vec::new();
        for i in 0..40u64 {
            let mut c = chunk(&block);
            let end = std::time::Duration::from_millis(i * 100 + 100);
            c.meta.timestamp = std::time::Duration::from_millis(i * 100);
            c.meta.end_timestamp = end;
            c.meta.frame_offset = i * u64::try_from(cf).unwrap();
            fed_ends.insert(end);
            if let Some(o) = fx.process(c) {
                emitted.push(o);
            }
        }
        while let Some(o) = fx.flush() {
            emitted.push(o);
        }
        assert!(!emitted.is_empty(), "stretch produced no output");
        for o in &emitted {
            assert_eq!(
                o.spec(),
                PcmSpec {
                    channels: CH,
                    sample_rate: SR
                },
                "spec (incl. sample rate) preserved verbatim"
            );
            assert_eq!(
                usize::try_from(o.meta.frames).unwrap(),
                o.samples().len() / channels,
                "frames recomputed to the actual output count"
            );
            assert!(
                fed_ends.contains(&o.meta.end_timestamp),
                "end_timestamp carried verbatim from an input chunk (source-track time)"
            );
        }
    }
}

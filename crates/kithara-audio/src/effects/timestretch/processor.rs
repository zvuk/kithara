use std::sync::Arc;
use std::sync::atomic::Ordering;

use kithara_bufpool::PcmPool;
use kithara_decode::{PcmChunk, PcmMeta, PcmSpec};
use portable_atomic::AtomicF32;
use tracing::warn;

use super::controls::StretchControls;
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

/// Pre-resampler time-stretch slot. Reads live key-lock, backend, and speed
/// from the shared [`StretchControls`] each chunk:
///
/// - key-lock **on**: drives the backend with the inverse stretch factor
///   (`1 / speed`), pitch held at `1.0`, and pins the resampler to `1.0`;
/// - key-lock **off**: a true pass-through — the chunk is forwarded untouched
///   and `speed` is routed to the resampler instead (pitch follows speed).
///
/// It owns the speed split: each chunk it writes the resampler's rate atomic
/// (`1.0` when key-locked, else `speed`). Both effects run sequentially on the
/// same worker thread, so the resampler always reads the value written for the
/// current chunk. See the crate `README.md` ("Live stretch controls").
pub struct TimeStretchProcessor {
    backend: Box<dyn StretchBackend>,
    /// Backend kind currently built; compared against `controls.backend()` to
    /// detect a live backend swap.
    current_kind: StretchBackendKind,
    controls: Arc<StretchControls>,
    /// Resampler rate atomic this slot drives. Shared with the resampler that
    /// follows it in the chain.
    resampler_rate: Arc<AtomicF32>,
    pool: PcmPool,
    spec: PcmSpec,
    /// Last stretch factor pushed to the backend; avoids redundant updates.
    applied_stretch: f64,
    /// Whether the previous chunk ran through the backend (key-lock on). Drives
    /// a clean backend reset on an on->off transition.
    active: bool,
    /// Most recent input meta, carried onto each output chunk.
    last_input_meta: Option<PcmMeta>,
    /// Interleaved output scratch, reused across calls (alloc-free steady state).
    scratch: Vec<f32>,
}

impl TimeStretchProcessor {
    /// Build the slot at the source `spec`, driven by the shared `controls`.
    /// `resampler_rate` is the atomic the following resampler reads; this slot
    /// is its sole writer.
    pub fn new(
        controls: Arc<StretchControls>,
        resampler_rate: Arc<AtomicF32>,
        spec: PcmSpec,
        pool: PcmPool,
    ) -> Self {
        let current_kind = controls.backend();
        let mut backend = build_backend(current_kind, spec);
        if let Err(e) = backend.set_pitch(1.0) {
            warn!(error = %e, "time-stretch set_pitch(1.0) failed");
        }
        let me = Self {
            backend,
            current_kind,
            controls,
            resampler_rate,
            pool,
            spec,
            applied_stretch: f64::NAN,
            active: false,
            last_input_meta: None,
            scratch: Vec::new(),
        };
        me.route_rate();
        me
    }

    /// Route the playback speed: the resampler stays at `1.0` while key-locked
    /// (this slot owns the tempo) and follows `speed` otherwise.
    fn route_rate(&self) {
        let rate = if self.controls.keylock() {
            1.0
        } else {
            self.controls.speed()
        };
        self.resampler_rate.store(rate, Ordering::Relaxed);
    }

    /// Pull the shared playback speed and update the backend stretch factor
    /// (`1 / speed`) when it has moved.
    fn sync_ratio(&mut self) {
        let speed = self.controls.speed().max(MIN_SPEED);
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

    /// Rebuild the backend for `kind` at the current `spec`, discarding any
    /// buffered state. Used on a live backend swap and on a source-spec change.
    fn rebuild_backend(&mut self, kind: StretchBackendKind) {
        self.backend = build_backend(kind, self.spec);
        if let Err(e) = self.backend.set_pitch(1.0) {
            warn!(error = %e, "time-stretch set_pitch(1.0) failed");
        }
        self.current_kind = kind;
        self.applied_stretch = f64::NAN;
        self.active = false;
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
        // Route the resampler rate first, before any accumulation early-return,
        // so a live key-lock toggle reaches the resampler on the same chunk.
        self.route_rate();

        let spec_changed = chunk.spec() != self.spec;
        if spec_changed {
            self.spec = chunk.spec();
        }
        let want_kind = self.controls.backend();
        if want_kind != self.current_kind || spec_changed {
            self.rebuild_backend(want_kind);
        }

        if !self.controls.keylock() {
            // Bypass: forward untouched. Clear any buffer left from a prior
            // key-locked run so the next on-transition starts clean.
            if self.active {
                self.backend.reset();
                self.applied_stretch = f64::NAN;
                self.active = false;
            }
            return Some(chunk);
        }

        self.active = true;
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
        self.active = false;
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

    fn spec() -> PcmSpec {
        PcmSpec {
            channels: CH,
            sample_rate: SR,
        }
    }

    /// Build a key-locked processor at `speed` on `kind`, plus the resampler
    /// rate atomic it drives.
    fn keylocked(kind: StretchBackendKind, speed: f32) -> (TimeStretchProcessor, Arc<AtomicF32>) {
        let controls = StretchControls::new(speed);
        controls.set_keylock(true);
        controls.set_backend(kind);
        let resampler_rate = Arc::new(AtomicF32::new(1.0));
        let fx = TimeStretchProcessor::new(
            controls,
            Arc::clone(&resampler_rate),
            spec(),
            PcmPool::default().clone(),
        );
        (fx, resampler_rate)
    }

    fn run(kind: StretchBackendKind, speed: f32, in_frames: usize) -> Vec<f32> {
        let input = sine(in_frames);
        let (mut fx, _rate) = keylocked(kind, speed);
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

    /// Half playback speed -> stretch 2.0 -> ~double duration, pitch held.
    /// Shared across every compiled-in backend.
    fn assert_half_speed_contract(kind: StretchBackendKind) {
        let channels = usize::from(CH);
        let in_frames = usize::try_from(SR).unwrap() * 2; // 2 s
        let out = run(kind, 0.5, in_frames);
        let out_frames = out.len() / channels;

        // Both C++ backends emit fixed-length output with leading latency-fill
        // (and bungee drops its tail), nudging the measured duration off an
        // exact 2x on a short clip — hence the ±10% band. Pitch is still
        // checked tightly below.
        assert!(
            out_frames * 10 >= in_frames * 18 && out_frames * 10 <= in_frames * 22,
            "{kind:?}: expected ~2x duration, got {out_frames} from {in_frames}"
        );

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

    #[cfg(feature = "stretch-signalsmith")]
    #[kithara::test]
    fn signalsmith_half_speed_and_unity_contracts() {
        assert_half_speed_contract(StretchBackendKind::Signalsmith);
        assert_unity_contract(StretchBackendKind::Signalsmith);
    }

    #[cfg(feature = "stretch-bungee")]
    #[kithara::test]
    fn bungee_half_speed_and_unity_contracts() {
        assert_half_speed_contract(StretchBackendKind::Bungee);
        assert_unity_contract(StretchBackendKind::Bungee);
    }

    #[kithara::test]
    fn output_meta_preserves_decoder_timeline() {
        let channels = usize::from(CH);
        let (mut fx, _rate) = keylocked(StretchBackendKind::default(), 0.5);
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

    /// Key-lock off is a true pass-through: PCM is forwarded byte-identical and
    /// the speed is routed to the resampler (which then moves pitch with it).
    #[kithara::test]
    fn bypass_forwards_input_and_routes_speed_to_resampler() {
        let controls = StretchControls::new(1.5);
        let rate = Arc::new(AtomicF32::new(1.0));
        let mut fx = TimeStretchProcessor::new(
            Arc::clone(&controls),
            Arc::clone(&rate),
            spec(),
            PcmPool::default().clone(),
        );
        let input = sine(8192);
        let block = 4096 * usize::from(CH);
        let mut out: Vec<f32> = Vec::new();
        for data in input.chunks(block) {
            if let Some(c) = fx.process(chunk(data)) {
                out.extend_from_slice(c.samples());
            }
        }
        assert_eq!(out, input, "key-lock off forwards PCM untouched");
        assert!(
            (rate.load(Ordering::Relaxed) - 1.5).abs() < 1e-6,
            "speed routed to the resampler in bypass"
        );
    }

    /// Key-lock on pins the resampler to unity (this slot owns the tempo).
    #[kithara::test]
    fn keylock_pins_resampler_to_unity() {
        let (mut fx, rate) = keylocked(StretchBackendKind::default(), 0.5);
        let _ = fx.process(chunk(&sine(4096)));
        assert!(
            (rate.load(Ordering::Relaxed) - 1.0).abs() < 1e-6,
            "resampler pinned to 1.0 while key-locked"
        );
    }

    /// Flipping key-lock mid-stream switches routing live: bypass + rate=speed
    /// before, pitch-preserving stretch + rate=1.0 after — no reload.
    #[kithara::test]
    fn live_keylock_toggle_switches_routing_and_stretches() {
        let controls = StretchControls::new(0.5);
        let rate = Arc::new(AtomicF32::new(1.0));
        let mut fx = TimeStretchProcessor::new(
            Arc::clone(&controls),
            Arc::clone(&rate),
            spec(),
            PcmPool::default().clone(),
        );
        let block = sine(4096);

        // Phase 1: key-lock off -> untouched passthrough, speed routed out.
        let off = fx.process(chunk(&block)).expect("bypass emits every chunk");
        assert_eq!(off.samples(), &block[..], "off: PCM forwarded untouched");
        assert!(
            (rate.load(Ordering::Relaxed) - 0.5).abs() < 1e-6,
            "off: speed routed to resampler"
        );

        // Phase 2: toggle on mid-stream.
        controls.set_keylock(true);
        let mut stretched: Vec<f32> = Vec::new();
        for _ in 0..24 {
            if let Some(c) = fx.process(chunk(&block)) {
                stretched.extend_from_slice(c.samples());
            }
        }
        while let Some(c) = fx.flush() {
            stretched.extend_from_slice(c.samples());
        }
        assert!(
            (rate.load(Ordering::Relaxed) - 1.0).abs() < 1e-6,
            "on: resampler pinned to unity"
        );
        let mono: Vec<f32> = stretched.iter().step_by(usize::from(CH)).copied().collect();
        assert!(mono.len() >= N, "on: not enough output for the FFT window");
        assert!(
            dominant_bin(&mono).abs_diff(expected_bin(F0)) <= 3,
            "on: pitch preserved after live toggle"
        );
    }

    /// Swapping the backend mid-stream keeps the stream flowing and pitch-locked.
    #[cfg(all(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
    #[kithara::test]
    fn live_backend_swap_continues_and_keeps_pitch() {
        let controls = StretchControls::new(0.5);
        controls.set_keylock(true);
        controls.set_backend(StretchBackendKind::Bungee);
        let rate = Arc::new(AtomicF32::new(1.0));
        let mut fx = TimeStretchProcessor::new(
            Arc::clone(&controls),
            rate,
            spec(),
            PcmPool::default().clone(),
        );
        let block = sine(4096);
        let mut out: Vec<f32> = Vec::new();
        for i in 0..24 {
            if i == 6 {
                controls.set_backend(StretchBackendKind::Signalsmith);
            }
            if let Some(c) = fx.process(chunk(&block)) {
                out.extend_from_slice(c.samples());
            }
        }
        while let Some(c) = fx.flush() {
            out.extend_from_slice(c.samples());
        }
        let mono: Vec<f32> = out.iter().step_by(usize::from(CH)).copied().collect();
        assert!(
            mono.len() >= N,
            "not enough output after swap for the FFT window"
        );
        assert!(
            dominant_bin(&mono).abs_diff(expected_bin(F0)) <= 3,
            "pitch preserved after live backend swap"
        );
    }
}

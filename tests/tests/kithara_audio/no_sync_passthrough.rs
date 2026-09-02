#![cfg(not(target_arch = "wasm32"))]

use kithara::{
    audio::{AudioConfig, AudioControl, AudioSession, NoResamplerBackend},
    platform::{
        CancelToken,
        sync::{
            Arc,
            atomic::{AtomicU8, Ordering},
        },
        time::{self, Duration, Instant},
    },
    play::{PlayWorker, PlayWorkerConfig, RegisteredAudio, TrackConfig, effects::AudioEffect},
    queue::{Queue, QueueConfig, Transition, test_utils::QueueProbe},
    signal::AudioChunk,
    stream::Stream,
    warp::{StretchControls, StretchKind, WarpConfig},
};
use kithara_integration_tests::{
    audio_artifact::write_audio_artifact,
    bufpool_ext::{TestPools, pools},
    cochlea::{
        CochleaReport, assert_oracle_load_bearing, continuity_failures, percentile_f32,
        time_stretch_failures,
    },
    kithara,
    memory_source::{MemStream, MemStreamConfig, MemorySource},
    offline::{OfflinePlayer, OfflinePlayerHarness, OfflinePlayerOptions, resource_from_reader},
};
use kithara_test_fixtures::{
    assets::{marked_sine_wav_a440_6s, sine_wav_a440_6s},
    signal::goertzel_magnitude,
};
use num_traits::ToPrimitive;
use serde::Serialize;

const SAMPLE_RATE: u32 = 44_100;
const CHANNELS: u16 = 2;
const BLOCK_FRAMES: usize = 512;
const SOURCE_SECONDS: usize = 6;
const WARMUP_BLOCKS: usize = 32;
const CAPTURE_BLOCKS: usize = 96;
const LOAD_INTERVAL_FRAMES: usize = BLOCK_FRAMES * 8;
const LOAD_BURST: Duration = Duration::from_millis(18);
const ACTIVE_SPEED: f32 = 0.8;
const TONE_HZ: f64 = 440.0;
const PITCH_SHIFTED_TONE_HZ: f64 = 352.0;
const TONE_DOMINANCE_RATIO: f64 = 8.0;
const CONTINUITY_RATIO: f32 = 3.0;
const CONTINUITY_SLACK: f32 = 2.0e-3;
const MAX_ACTIVE_STEREO_DELTA: f32 = 1.0e-3;
const RATE_MARKER_START_FRAMES: [usize; 2] = [17_640, 35_280];
const RATE_MARKER_FRAMES: usize = 2_205;
const RATE_MARKER_PEAK: i16 = 2_000;
const RATE_WINDOW_FRAMES: usize = 256;
const RATE_MARKER_JOIN_FRAMES: usize = RATE_WINDOW_FRAMES * 2;
const RATE_TIMING_TOLERANCE_FRAMES: usize = BLOCK_FRAMES * 2;

const LOAD_WARMUP: u8 = 0;
const LOAD_CAPTURE: u8 = 1;
const LOAD_CAPTURE_SEEN: u8 = 2;
const LOAD_DONE: u8 = 3;

const WAV_HEADER_BYTES: usize = 44;
const SAMPLE_BYTES: usize = 2;

fn source_frames() -> usize {
    usize::try_from(SAMPLE_RATE).expect("sample rate fits usize") * SOURCE_SECONDS
}

/// The plain tone this test measures, with the fixture bound to its budget.
fn source_pcm() -> &'static [u8] {
    let bytes = sine_wav_a440_6s().bytes();
    assert_eq!(
        bytes.len(),
        WAV_HEADER_BYTES + source_frames() * usize::from(CHANNELS) * SAMPLE_BYTES,
        "fixture sine_wav_a440_6s no longer matches this test's frame budget",
    );
    bytes
}

/// The same tone carrying the source-time markers `marker_timing` looks for.
fn marked_source_pcm() -> &'static [u8] {
    let bytes = marked_sine_wav_a440_6s().bytes();
    assert_eq!(bytes.len(), source_pcm().len());
    for start in RATE_MARKER_START_FRAMES {
        assert!(
            peak_amplitude(bytes, start, RATE_MARKER_FRAMES) <= RATE_MARKER_PEAK,
            "fixture marked_sine_wav_a440_6s is loud across the marker at frame {start}",
        );
        assert!(
            peak_amplitude(bytes, start - RATE_MARKER_FRAMES, RATE_MARKER_FRAMES)
                > RATE_MARKER_PEAK,
            "fixture marked_sine_wav_a440_6s has no amplitude step into the marker at frame {start}",
        );
    }
    bytes
}

/// Loudest absolute sample across `frames` of a 16-bit interleaved WAV.
fn peak_amplitude(wav: &[u8], start_frame: usize, frames: usize) -> i16 {
    let channels = usize::from(CHANNELS);
    wav[WAV_HEADER_BYTES..]
        .chunks_exact(SAMPLE_BYTES)
        .skip(start_frame * channels)
        .take(frames * channels)
        .map(|sample| i16::from_le_bytes([sample[0], sample[1]]).saturating_abs())
        .max()
        .expect("the window is inside the fixture")
}

struct LoadProbe {
    phase: AtomicU8,
}

impl LoadProbe {
    // Not const: on the loom lane the platform alias resolves to loom's atomic.
    fn new() -> Self {
        Self {
            phase: AtomicU8::new(LOAD_WARMUP),
        }
    }

    fn start_capture(&self) {
        self.phase.store(LOAD_CAPTURE, Ordering::Release);
    }

    fn observe_burst(&self) {
        let _ = self.phase.compare_exchange(
            LOAD_CAPTURE,
            LOAD_CAPTURE_SEEN,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }

    fn finish_capture(&self) -> bool {
        self.phase.swap(LOAD_DONE, Ordering::AcqRel) == LOAD_CAPTURE_SEEN
    }
}

struct BurstLoadEffect {
    frames: usize,
    probe: Arc<LoadProbe>,
}

impl BurstLoadEffect {
    fn new(probe: Arc<LoadProbe>) -> Self {
        Self { frames: 0, probe }
    }
}

impl AudioEffect for BurstLoadEffect {
    fn flush(&mut self) -> Option<AudioChunk> {
        None
    }

    fn held_source_frames(&self) -> u64 {
        0
    }

    fn process(&mut self, chunk: AudioChunk) -> Option<AudioChunk> {
        self.frames = self.frames.saturating_add(chunk.frames());
        if self.frames >= LOAD_INTERVAL_FRAMES {
            self.frames -= LOAD_INTERVAL_FRAMES;
            self.probe.observe_burst();
            let deadline = Instant::now() + LOAD_BURST;
            while Instant::now() < deadline {
                std::hint::spin_loop();
            }
        }
        Some(chunk)
    }

    fn reset(&mut self) {
        self.frames = 0;
    }
}

#[derive(Debug)]
struct RealtimeCapture {
    pcm: Vec<f32>,
    warmup_decode_errors: u64,
    warmup_underruns: u64,
    decode_errors: u64,
    underruns: u64,
    load_observed_during_capture: bool,
}

#[derive(Serialize)]
struct CaptureMetrics {
    warmup_decode_errors: u64,
    warmup_underruns: u64,
    decode_errors: u64,
    underruns: u64,
    load_observed_during_capture: bool,
}

#[derive(Debug, Serialize)]
struct SineFit {
    amplitude: f64,
    dc: f64,
    non_finite_samples: usize,
    residual_ratio: f64,
    stereo_mismatches: usize,
    max_stereo_delta: f32,
}

#[derive(Debug, Serialize)]
struct ChannelContinuity {
    channel: usize,
    peak_step: f32,
    background_step: f32,
    step_limit: f32,
    peak_residual: f32,
    background_residual: f32,
    residual_limit: f32,
}

#[derive(Debug, Serialize)]
struct MarkerTiming {
    background_rms: f32,
    expected_interval_frames: usize,
    marker_frames: [usize; 2],
    marker_rms: f32,
    measured_interval_frames: usize,
}

impl From<&RealtimeCapture> for CaptureMetrics {
    fn from(capture: &RealtimeCapture) -> Self {
        Self {
            warmup_decode_errors: capture.warmup_decode_errors,
            warmup_underruns: capture.warmup_underruns,
            decode_errors: capture.decode_errors,
            underruns: capture.underruns,
            load_observed_during_capture: capture.load_observed_during_capture,
        }
    }
}

#[derive(Serialize)]
struct PassthroughManifest<'a> {
    case: &'static str,
    backend: &'a str,
    sample_rate: u32,
    channels: u16,
    block_frames: usize,
    baseline: CaptureMetrics,
    unity: CaptureMetrics,
    unity_under_load: CaptureMetrics,
    baseline_source_fit: &'a SineFit,
    baseline_cochlea: &'a CochleaReport,
    unity_cochlea: &'a CochleaReport,
    unity_under_load_cochlea: &'a CochleaReport,
    failures: &'a [String],
}

#[derive(Serialize)]
struct ActiveStretchManifest<'a> {
    case: &'static str,
    backend: &'a str,
    speed: f32,
    sample_rate: u32,
    channels: u16,
    control: CaptureMetrics,
    candidate: CaptureMetrics,
    control_cochlea: &'a CochleaReport,
    candidate_cochlea: &'a CochleaReport,
    candidate_sine_fit: &'a SineFit,
    candidate_continuity: &'a [ChannelContinuity],
    rate_control: &'a MarkerTiming,
    rate_candidate: &'a MarkerTiming,
    failures: &'a [String],
}

fn measure_quiet_sine(samples: &[f32]) -> SineFit {
    let channels = usize::from(CHANNELS);
    let frames = samples.len() / channels;
    let mut non_finite_samples = 0;
    let mut stereo_mismatches = 0;
    let mut max_stereo_delta = 0.0_f32;
    let mut sum = 0.0;
    let mut sum_sin = 0.0;
    let mut sum_cos = 0.0;

    for frame in 0..frames {
        let left = samples[frame * channels];
        non_finite_samples += usize::from(!left.is_finite());
        for sample in &samples[frame * channels + 1..(frame + 1) * channels] {
            stereo_mismatches += usize::from(sample.to_bits() != left.to_bits());
            max_stereo_delta = max_stereo_delta.max((*sample - left).abs());
        }
        let phase =
            std::f64::consts::TAU * TONE_HZ * frame.to_f64().expect("capture frame fits f64")
                / f64::from(SAMPLE_RATE);
        let sample = f64::from(left);
        sum += sample;
        sum_sin += sample * phase.sin();
        sum_cos += sample * phase.cos();
    }

    let count = frames.to_f64().expect("capture length fits f64");
    let dc = sum / count;
    let sin_coefficient = 2.0 * sum_sin / count;
    let cos_coefficient = 2.0 * sum_cos / count;
    let amplitude = sin_coefficient.hypot(cos_coefficient);
    let mut signal_energy = 0.0;
    let mut residual_energy = 0.0;
    for frame in 0..frames {
        let phase =
            std::f64::consts::TAU * TONE_HZ * frame.to_f64().expect("capture frame fits f64")
                / f64::from(SAMPLE_RATE);
        let sample = f64::from(samples[frame * channels]);
        let centered = sample - dc;
        let fitted = sin_coefficient * phase.sin() + cos_coefficient * phase.cos();
        signal_energy += centered * centered;
        residual_energy += (centered - fitted) * (centered - fitted);
    }

    SineFit {
        amplitude,
        dc,
        non_finite_samples,
        residual_ratio: (residual_energy / signal_energy).sqrt(),
        stereo_mismatches,
        max_stereo_delta,
    }
}

fn stretch_controls(stretch: Option<(StretchKind, f32)>) -> Arc<StretchControls> {
    stretch.map_or_else(
        || StretchControls::new(1.0),
        |(backend, speed)| {
            let controls = StretchControls::new(speed);
            controls.set_backend(backend);
            controls.set_keylock(true);
            controls
        },
    )
}

fn audio_config(
    source: &[u8],
    stretch: Arc<StretchControls>,
    effects: Vec<Box<dyn AudioEffect>>,
) -> TrackConfig<MemStream, NoResamplerBackend> {
    let stream = MemStreamConfig {
        source: Some(MemorySource::new(source.to_vec())),
        event_bus: None,
    };

    let audio = AudioConfig::<MemStream>::for_stream(stream)
        .hint("wav".to_owned())
        .build();
    TrackConfig::for_audio(audio)
        .warp(WarpConfig::builder().stretch(stretch).build())
        .effects(effects)
        .build()
}

async fn wait_for_preload(audio: &RegisteredAudio<Stream<MemStream>, TestPools>) {
    let gate = audio
        .preload_gate()
        .expect("worker-backed audio exposes a preload gate");
    time::timeout(
        Duration::from_secs(5),
        gate.wait_for_epoch(audio.preload_epoch()),
    )
    .await
    .expect("audio preload gate must open");
}

async fn render_passthrough(
    source: &[u8],
    stretch: Option<(StretchKind, f32)>,
    with_load: bool,
) -> RealtimeCapture {
    let load_probe = Arc::new(LoadProbe::new());
    let worker = PlayWorker::new(
        PlayWorkerConfig::builder(pools())
            .cancel(CancelToken::never())
            .build(),
    );
    let target_stretch = stretch_controls(stretch);
    let mut target_audio = worker
        .open(audio_config(source, target_stretch, Vec::new()))
        .await
        .expect("target audio construction");
    wait_for_preload(&target_audio).await;
    target_audio.preload().expect("target preload");

    let mut load_audio = if with_load {
        let mut audio = worker
            .open(audio_config(
                source,
                stretch_controls(None),
                vec![Box::new(BurstLoadEffect::new(Arc::clone(&load_probe)))],
            ))
            .await
            .expect("load audio construction");
        wait_for_preload(&audio).await;
        audio.preload().expect("load preload");
        Some(audio)
    } else {
        None
    };

    let mut target = OfflinePlayer::new(SAMPLE_RATE);
    target.set_fade_duration(0.0);
    target.load_and_fadein(resource_from_reader(target_audio), "passthrough-target");
    let mut load = load_audio.take().map(|audio| {
        let mut player = OfflinePlayer::new(SAMPLE_RATE);
        player.set_fade_duration(0.0);
        player.load_and_fadein(resource_from_reader(audio), "shared-worker-load");
        player
    });
    let block_period = Duration::from_secs_f64(
        f64::from(u32::try_from(BLOCK_FRAMES).expect("block size fits u32"))
            / f64::from(SAMPLE_RATE),
    );

    let metrics_before_warmup = target.metrics();
    for _ in 0..WARMUP_BLOCKS {
        let started = Instant::now();
        if let Some(player) = load.as_mut() {
            let _ = player.render(BLOCK_FRAMES);
        }
        let _ = target.render(BLOCK_FRAMES);
        time::sleep(block_period.saturating_sub(started.elapsed())).await;
    }
    let metrics_before_capture = target.metrics();
    let warmup_decode_errors = metrics_before_capture
        .decode_errors()
        .saturating_sub(metrics_before_warmup.decode_errors());
    let warmup_underruns = metrics_before_capture
        .underruns()
        .saturating_sub(metrics_before_warmup.underruns());
    let mut pcm = Vec::with_capacity(CAPTURE_BLOCKS * BLOCK_FRAMES * usize::from(CHANNELS));
    load_probe.start_capture();
    for _ in 0..CAPTURE_BLOCKS {
        let started = Instant::now();
        if let Some(player) = load.as_mut() {
            let _ = player.render(BLOCK_FRAMES);
        }
        pcm.extend(target.render(BLOCK_FRAMES));
        time::sleep(block_period.saturating_sub(started.elapsed())).await;
    }
    let load_observed_during_capture = load_probe.finish_capture();
    let metrics_after_capture = target.metrics();

    RealtimeCapture {
        pcm,
        warmup_decode_errors,
        warmup_underruns,
        decode_errors: metrics_after_capture
            .decode_errors()
            .saturating_sub(metrics_before_capture.decode_errors()),
        underruns: metrics_after_capture
            .underruns()
            .saturating_sub(metrics_before_capture.underruns()),
        load_observed_during_capture,
    }
}

async fn render_queue_passthrough(source: &[u8], stretch: Option<(StretchKind, f32)>) -> Vec<f32> {
    let stretch = stretch_controls(stretch);
    let harness = OfflinePlayerHarness::with_sample_rate(
        OfflinePlayerOptions::builder()
            .crossfade_duration(0.0)
            .warp(WarpConfig::builder().stretch(Arc::clone(&stretch)).build())
            .build(),
        SAMPLE_RATE,
    );
    let worker = harness.with_player(|player| player.worker().clone());
    let mut audio = worker
        .open(audio_config(source, stretch, Vec::new()))
        .await
        .expect("queue audio construction");
    wait_for_preload(&audio).await;
    audio.preload().expect("queue audio preload");

    let queue = Queue::new(
        QueueConfig::builder()
            .player(harness.take_player())
            .should_autoplay(false)
            .build(),
    );
    let id = queue.insert_loaded_for_test(resource_from_reader(audio));
    queue
        .select(id, Transition::None)
        .expect("select queue passthrough track");

    let block_period = Duration::from_secs_f64(
        f64::from(u32::try_from(BLOCK_FRAMES).expect("block size fits u32"))
            / f64::from(SAMPLE_RATE),
    );
    for _ in 0..WARMUP_BLOCKS {
        let started = Instant::now();
        queue.tick().expect("tick queue during warmup");
        let _ = harness.render(BLOCK_FRAMES);
        time::sleep(block_period.saturating_sub(started.elapsed())).await;
    }

    let mut pcm = Vec::with_capacity(CAPTURE_BLOCKS * BLOCK_FRAMES * usize::from(CHANNELS));
    for _ in 0..CAPTURE_BLOCKS {
        let started = Instant::now();
        queue.tick().expect("tick queue during capture");
        pcm.extend(harness.render(BLOCK_FRAMES));
        time::sleep(block_period.saturating_sub(started.elapsed())).await;
    }
    pcm
}

fn first_sample_mismatch(candidate: &[f32], control: &[f32]) -> Option<usize> {
    candidate
        .iter()
        .zip(control)
        .position(|(candidate, control)| candidate.to_bits() != control.to_bits())
        .or_else(|| (candidate.len() != control.len()).then(|| candidate.len().min(control.len())))
}

fn first_channel(samples: &[f32]) -> Vec<f32> {
    samples
        .chunks_exact(usize::from(CHANNELS))
        .map(|frame| frame[0])
        .collect()
}

fn tone_dominates(samples: &[f32], tone_hz: f64, competing_hz: f64) -> bool {
    let tone = goertzel_magnitude(samples, tone_hz, SAMPLE_RATE);
    let competing = goertzel_magnitude(samples, competing_hz, SAMPLE_RATE);
    tone > competing * TONE_DOMINANCE_RATIO
}

fn tone_fixture(frequency_hz: f64, frames: usize) -> Vec<f32> {
    let phase_step = std::f64::consts::TAU * frequency_hz / f64::from(SAMPLE_RATE);
    (0..frames)
        .map(|frame| {
            (phase_step * frame.to_f64().expect("fixture frame fits f64"))
                .sin()
                .to_f32()
                .expect("unit sine sample fits f32")
                * 0.5
        })
        .collect()
}

fn expected_marker_interval(speed: f32) -> usize {
    let source_interval = RATE_MARKER_START_FRAMES[1]
        .checked_sub(RATE_MARKER_START_FRAMES[0])
        .expect("rate markers are ordered");
    (source_interval.to_f64().expect("marker interval fits f64") / f64::from(speed))
        .round()
        .to_usize()
        .expect("scaled marker interval fits usize")
}

fn marker_timing(samples: &[f32], speed: f32) -> MarkerTiming {
    let channels = usize::from(CHANNELS);
    let window_samples = RATE_WINDOW_FRAMES
        .checked_mul(channels)
        .expect("rate-marker window fits usize");
    let windows: Vec<(usize, f32)> = samples
        .chunks_exact(window_samples)
        .enumerate()
        .map(|(window, samples)| {
            let energy = samples
                .chunks_exact(channels)
                .map(|frame| frame[0] * frame[0])
                .sum::<f32>();
            let rms = (energy
                / RATE_WINDOW_FRAMES
                    .to_f32()
                    .expect("rate-marker window fits f32"))
            .sqrt();
            (window * RATE_WINDOW_FRAMES + RATE_WINDOW_FRAMES / 2, rms)
        })
        .collect();
    assert!(!windows.is_empty(), "rate-marker capture has RMS windows");
    let mut rms_values: Vec<f32> = windows.iter().map(|(_, rms)| *rms).collect();
    rms_values.sort_by(f32::total_cmp);
    let background_rms = rms_values[rms_values.len() / 2];
    let marker_threshold = background_rms / 4.0;
    let mut groups: Vec<Vec<(usize, f32)>> = Vec::new();
    for window in windows
        .iter()
        .copied()
        .filter(|(_, rms)| *rms < marker_threshold)
    {
        let contiguous = groups
            .last()
            .and_then(|group| group.last())
            .is_some_and(|(frame, _)| frame.saturating_add(RATE_MARKER_JOIN_FRAMES) >= window.0);
        if contiguous {
            groups
                .last_mut()
                .expect("a contiguous marker group exists")
                .push(window);
        } else {
            groups.push(vec![window]);
        }
    }
    assert_eq!(
        groups.len(),
        RATE_MARKER_START_FRAMES.len(),
        "rate oracle must resolve exactly two source-time markers: groups={groups:?}, background_rms={background_rms}"
    );
    let marker_frames = std::array::from_fn(|index| {
        let group = &groups[index];
        let first = group.first().expect("marker group is non-empty").0;
        let last = group.last().expect("marker group is non-empty").0;
        first + last.saturating_sub(first) / 2
    });
    let marker_rms = groups
        .iter()
        .map(|group| {
            group
                .iter()
                .map(|(_, rms)| *rms)
                .min_by(f32::total_cmp)
                .expect("marker group has an RMS minimum")
        })
        .fold(0.0_f32, f32::max);
    let measured_interval_frames = marker_frames[1]
        .checked_sub(marker_frames[0])
        .expect("detected rate markers are ordered");
    MarkerTiming {
        background_rms,
        expected_interval_frames: expected_marker_interval(speed),
        marker_frames,
        marker_rms,
        measured_interval_frames,
    }
}

fn marker_timing_failures(label: &str, timing: &MarkerTiming) -> Vec<String> {
    let mut failures = Vec::new();
    if timing.marker_rms * 4.0 >= timing.background_rms {
        failures.push(format!(
            "{label}: source-time marker is not distinct: marker_rms={:.6}, background_rms={:.6}",
            timing.marker_rms, timing.background_rms,
        ));
    }
    let error = timing
        .measured_interval_frames
        .abs_diff(timing.expected_interval_frames);
    if error > RATE_TIMING_TOLERANCE_FRAMES {
        failures.push(format!(
            "{label}: source-time marker interval is {error} frames from the expected scale: measured={}, expected={}, markers={:?}, tolerance={RATE_TIMING_TOLERANCE_FRAMES}",
            timing.measured_interval_frames, timing.expected_interval_frames, timing.marker_frames,
        ));
    }
    failures
}

fn frame_continuity(samples: &[f32]) -> (Vec<ChannelContinuity>, Vec<String>) {
    let channels = usize::from(CHANNELS);
    let frames = samples.len() / channels;
    assert!(frames >= 3, "continuity oracle needs at least three frames");
    let omega = std::f32::consts::TAU * TONE_HZ.to_f32().expect("fixture frequency fits f32")
        / SAMPLE_RATE.to_f32().expect("fixture sample rate fits f32");
    let recurrence = 2.0 * omega.cos();
    let mut reports = Vec::with_capacity(channels);
    let mut failures = Vec::new();

    for channel in 0..channels {
        let sample = |frame: usize| samples[frame * channels + channel];
        let mut steps = Vec::with_capacity(frames.saturating_sub(1));
        let mut residuals = Vec::with_capacity(frames.saturating_sub(2));
        let mut peak_step = 0.0_f32;
        let mut peak_residual = 0.0_f32;
        for frame in 1..frames {
            let step = (sample(frame) - sample(frame - 1)).abs();
            peak_step = peak_step.max(step);
            steps.push(step);
            if frame >= 2 {
                let residual =
                    (sample(frame) - recurrence * sample(frame - 1) + sample(frame - 2)).abs();
                peak_residual = peak_residual.max(residual);
                residuals.push(residual);
            }
        }
        let background_step = percentile_f32(&mut steps, 99, 100);
        let background_residual = percentile_f32(&mut residuals, 99, 100);
        let step_limit = background_step.mul_add(CONTINUITY_RATIO, CONTINUITY_SLACK);
        let residual_limit = background_residual.mul_add(CONTINUITY_RATIO, CONTINUITY_SLACK);
        if peak_step > step_limit {
            failures.push(format!(
                "channel {channel}: sample step {peak_step:.6} exceeds {step_limit:.6}",
            ));
        }
        if peak_residual > residual_limit {
            failures.push(format!(
                "channel {channel}: sine residual {peak_residual:.6} exceeds {residual_limit:.6}",
            ));
        }
        reports.push(ChannelContinuity {
            channel,
            peak_step,
            background_step,
            step_limit,
            peak_residual,
            background_residual,
            residual_limit,
        });
    }

    (reports, failures)
}

fn assert_frame_oracle_load_bearing(control: &[f32]) {
    let channels = usize::from(CHANNELS);
    let frames = control.len() / channels;
    assert!(
        frame_continuity(control).1.is_empty(),
        "frame oracle control must be continuous"
    );

    let search_start = frames / 3;
    let search_end = frames * 2 / 3;
    let click_frame = (search_start..search_end)
        .find(|&frame| (0.2..=0.4).contains(&control[frame * channels].abs()))
        .expect("active fixture contains a sub-clipping click position");
    let mut clicked = control.to_vec();
    for sample in &mut clicked[click_frame * channels..(click_frame + 1) * channels] {
        *sample = -*sample;
    }
    assert!(
        !frame_continuity(&clicked).1.is_empty(),
        "frame oracle accepted an injected sub-clipping one-frame click"
    );

    let mut comb = control.to_vec();
    for frame in (BLOCK_FRAMES..frames).step_by(BLOCK_FRAMES) {
        for sample in &mut comb[frame * channels..(frame + 1) * channels] {
            *sample = -*sample;
        }
    }
    assert!(
        !frame_continuity(&comb).1.is_empty(),
        "frame oracle accepted recurring block-boundary clicks"
    );

    let held_frame = frames / 2;
    let mut held = control.to_vec();
    for channel in 0..channels {
        held[held_frame * channels + channel] = held[(held_frame - 1) * channels + channel];
    }
    assert!(
        !frame_continuity(&held).1.is_empty(),
        "frame oracle accepted one held PCM frame"
    );

    let mut divergent = control.to_vec();
    divergent[held_frame * channels + 1] += MAX_ACTIVE_STEREO_DELTA * 2.0;
    assert!(
        measure_quiet_sine(&divergent).max_stereo_delta > MAX_ACTIVE_STEREO_DELTA,
        "stereo oracle accepted a channel-only frame error"
    );
}

#[kithara::test(
    tokio,
    flash(false),
    serial,
    timeout(Duration::from_secs(30)),
    hang_timeout_secs(5)
)]
#[case(StretchKind::Signalsmith)]
#[cfg_attr(
    not(all(target_os = "windows", target_env = "msvc")),
    case(StretchKind::Bungee)
)]
async fn no_sync_unity_player_and_queue_playback_is_bit_exact_and_cochlea_clean(
    #[case] backend: StretchKind,
) {
    run_no_sync_passthrough(backend, false).await;
}

#[kithara::test(
    tokio,
    flash(false),
    serial,
    timeout(Duration::from_secs(30)),
    hang_timeout_secs(5)
)]
#[case(StretchKind::Signalsmith)]
#[cfg_attr(
    not(all(target_os = "windows", target_env = "msvc")),
    case(StretchKind::Bungee)
)]
async fn no_sync_active_keylock_is_continuous_and_preserves_pitch(#[case] backend: StretchKind) {
    run_active_stretch(backend, false).await;
}

#[kithara::test(
    tokio,
    flash(false),
    serial,
    timeout(Duration::from_secs(60)),
    hang_timeout_secs(5)
)]
#[case(StretchKind::Signalsmith)]
#[cfg_attr(
    not(all(target_os = "windows", target_env = "msvc")),
    case(StretchKind::Bungee)
)]
#[ignore = "writes opt-in listening artifacts; run explicitly with KITHARA_AUDIO_ARTIFACT_DIR"]
async fn record_no_sync_unity_playback_artifacts(#[case] backend: StretchKind) {
    run_no_sync_passthrough(backend, true).await;
}

#[kithara::test(
    tokio,
    flash(false),
    serial,
    timeout(Duration::from_secs(60)),
    hang_timeout_secs(5)
)]
#[case(StretchKind::Signalsmith)]
#[cfg_attr(
    not(all(target_os = "windows", target_env = "msvc")),
    case(StretchKind::Bungee)
)]
#[ignore = "writes opt-in listening artifacts; run explicitly with KITHARA_AUDIO_ARTIFACT_DIR"]
async fn record_no_sync_active_keylock_artifacts(#[case] backend: StretchKind) {
    run_active_stretch(backend, true).await;
}

async fn run_no_sync_passthrough(backend: StretchKind, record_artifacts: bool) {
    let channels = usize::from(CHANNELS);
    let source = source_pcm();
    let baseline = render_passthrough(source, None, false).await;
    let baseline_report = CochleaReport::measure(&baseline.pcm, CHANNELS, SAMPLE_RATE);
    let baseline_source_fit = measure_quiet_sine(&baseline.pcm);
    let unity = render_passthrough(source, Some((backend, 1.0)), false).await;
    let unity_report = CochleaReport::measure(&unity.pcm, CHANNELS, SAMPLE_RATE);
    let loaded = render_passthrough(source, Some((backend, 1.0)), true).await;
    let loaded_report = CochleaReport::measure(&loaded.pcm, CHANNELS, SAMPLE_RATE);
    let queue_baseline = render_queue_passthrough(source, None).await;
    let queue_unity = render_queue_passthrough(source, Some((backend, 1.0))).await;
    let mut failures = Vec::new();
    for (label, pcm) in [
        ("queue effect-free", queue_baseline.as_slice()),
        ("queue unity", queue_unity.as_slice()),
    ] {
        if let Some(sample) = first_sample_mismatch(pcm, &baseline.pcm) {
            failures.push(format!(
                "{label}: PCM differs from direct playback at sample {sample} (frame {})",
                sample / channels,
            ));
        }
    }
    if !baseline.pcm.iter().any(|sample| sample.abs() > 0.25) {
        failures.push("effect-free control contains no audible PCM".to_owned());
    }
    if baseline_source_fit.non_finite_samples != 0
        || baseline_source_fit.stereo_mismatches != 0
        || !(0.47..=0.50).contains(&baseline_source_fit.amplitude)
        || baseline_source_fit.dc.abs() > 0.002
        || baseline_source_fit.residual_ratio > 0.01
    {
        failures.push(format!(
            "effect-free control does not match the independent 440 Hz source oracle: {baseline_source_fit:?}",
        ));
    }
    if baseline_report.silent_segments != 0 {
        failures.push(format!(
            "effect-free: {} silent Cochlea segment(s)",
            baseline_report.silent_segments,
        ));
    }
    if baseline_report.onset_count() != 0 {
        failures.push(format!(
            "effect-free: {} unexpected Cochlea onset(s)",
            baseline_report.onset_count(),
        ));
    }
    if baseline_report.clipped_samples != 0 || baseline_report.true_peak_over_0dbtp {
        failures.push(format!(
            "effect-free: clipping evidence samples={}, true_peak_over_0dbtp={}",
            baseline_report.clipped_samples, baseline_report.true_peak_over_0dbtp,
        ));
    }

    for (label, capture, report) in [
        ("effect-free", &baseline, &baseline_report),
        ("unity", &unity, &unity_report),
        ("unity+load", &loaded, &loaded_report),
    ] {
        let non_finite = capture
            .pcm
            .iter()
            .filter(|sample| !sample.is_finite())
            .count();
        if non_finite != 0 {
            failures.push(format!("{label}: {non_finite} non-finite PCM sample(s)"));
        }
        if capture.warmup_decode_errors != 0 || capture.decode_errors != 0 {
            failures.push(format!(
                "{label}: decode errors warmup={}, capture={}",
                capture.warmup_decode_errors, capture.decode_errors,
            ));
        }
        if capture.warmup_underruns != 0 || capture.underruns != 0 {
            failures.push(format!(
                "{label}: underruns warmup={}, capture={}",
                capture.warmup_underruns, capture.underruns,
            ));
        }
        if label != "effect-free" {
            failures.extend(continuity_failures(label, report, &baseline_report));
            if let Some(sample) = first_sample_mismatch(&capture.pcm, &baseline.pcm) {
                failures.push(format!(
                    "{label}: PCM differs at sample {sample} (frame {})",
                    sample / channels,
                ));
            }
        }
    }
    if !loaded.load_observed_during_capture {
        failures.push("unity+load: no bounded shared-worker burst began during capture".to_owned());
    }

    let backend_label = backend.to_string().to_ascii_lowercase();
    let manifest = PassthroughManifest {
        case: "no-sync-unity-passthrough",
        backend: &backend_label,
        sample_rate: SAMPLE_RATE,
        channels: CHANNELS,
        block_frames: BLOCK_FRAMES,
        baseline: CaptureMetrics::from(&baseline),
        unity: CaptureMetrics::from(&unity),
        unity_under_load: CaptureMetrics::from(&loaded),
        baseline_source_fit: &baseline_source_fit,
        baseline_cochlea: &baseline_report,
        unity_cochlea: &unity_report,
        unity_under_load_cochlea: &loaded_report,
        failures: &failures,
    };
    if record_artifacts {
        let artifact_case = format!("no-sync-unity-passthrough-{backend_label}");
        let written = write_audio_artifact(
            &artifact_case,
            SAMPLE_RATE,
            CHANNELS,
            &[
                ("effect-free-control", &baseline.pcm),
                ("unity", &unity.pcm),
                ("unity-under-load", &loaded.pcm),
            ],
            &manifest,
        )
        .expect("no-SYNC audio artifact write");
        assert!(
            written.is_some(),
            "KITHARA_AUDIO_ARTIFACT_DIR must be set for the artifact recorder"
        );
    }

    assert_oracle_load_bearing(&baseline.pcm, CHANNELS, SAMPLE_RATE, BLOCK_FRAMES);
    assert!(
        failures.is_empty(),
        "no-SYNC playback was not transparent: {}\nbaseline={baseline_report:?}\nunity={unity_report:?}\nunity+load={loaded_report:?}",
        failures.join("; "),
    );
}

async fn run_active_stretch(backend: StretchKind, record_artifacts: bool) {
    let source = source_pcm();
    let marker_source = marked_source_pcm();
    let control = render_passthrough(source, None, false).await;
    let candidate = render_passthrough(source, Some((backend, ACTIVE_SPEED)), false).await;
    let rate_control = render_passthrough(marker_source, None, false).await;
    let rate_candidate =
        render_passthrough(marker_source, Some((backend, ACTIVE_SPEED)), false).await;
    let control_report = CochleaReport::measure(&control.pcm, CHANNELS, SAMPLE_RATE);
    let candidate_report = CochleaReport::measure(&candidate.pcm, CHANNELS, SAMPLE_RATE);
    let candidate_sine_fit = measure_quiet_sine(&candidate.pcm);
    let (candidate_continuity, continuity_failures) = frame_continuity(&candidate.pcm);
    let mut failures = time_stretch_failures("active keylock", &candidate_report, &control_report);
    failures.extend(continuity_failures);
    let rate_control_timing = marker_timing(&rate_control.pcm, 1.0);
    let rate_candidate_timing = marker_timing(&rate_candidate.pcm, ACTIVE_SPEED);
    failures.extend(marker_timing_failures(
        "unity rate control",
        &rate_control_timing,
    ));
    failures.extend(marker_timing_failures(
        "active keylock rate",
        &rate_candidate_timing,
    ));
    let unchanged_rate_negative = marker_timing(&rate_control.pcm, ACTIVE_SPEED);
    assert!(
        !marker_timing_failures("unchanged-rate negative control", &unchanged_rate_negative)
            .is_empty(),
        "rate oracle accepted unity playback as {ACTIVE_SPEED}x"
    );

    for (label, capture) in [
        ("effect-free", &control),
        ("active keylock", &candidate),
        ("rate control", &rate_control),
        ("active rate", &rate_candidate),
    ] {
        let non_finite = capture
            .pcm
            .iter()
            .filter(|sample| !sample.is_finite())
            .count();
        if non_finite != 0 {
            failures.push(format!("{label}: {non_finite} non-finite PCM sample(s)"));
        }
        if capture.warmup_decode_errors != 0 || capture.decode_errors != 0 {
            failures.push(format!(
                "{label}: decode errors warmup={}, capture={}",
                capture.warmup_decode_errors, capture.decode_errors,
            ));
        }
        if capture.warmup_underruns != 0 || capture.underruns != 0 {
            failures.push(format!(
                "{label}: underruns warmup={}, capture={}",
                capture.warmup_underruns, capture.underruns,
            ));
        }
    }

    if first_sample_mismatch(&candidate.pcm, &control.pcm).is_none() {
        failures.push("active keylock remained on the unity passthrough path".to_owned());
    }
    if candidate_sine_fit.non_finite_samples != 0
        || candidate_sine_fit.max_stereo_delta > MAX_ACTIVE_STEREO_DELTA
        || !(0.47..=0.51).contains(&candidate_sine_fit.amplitude)
        || candidate_sine_fit.dc.abs() > 0.002
        || candidate_sine_fit.residual_ratio > 0.01
    {
        failures.push(format!(
            "active keylock violates the independent 440 Hz signal oracle: {candidate_sine_fit:?}",
        ));
    }
    let candidate_mono = first_channel(&candidate.pcm);
    if !tone_dominates(&candidate_mono, TONE_HZ, PITCH_SHIFTED_TONE_HZ) {
        failures.push(format!(
            "active keylock did not preserve {TONE_HZ} Hz over the pitch-shifted {PITCH_SHIFTED_TONE_HZ} Hz control",
        ));
    }
    let wrong_pitch = tone_fixture(PITCH_SHIFTED_TONE_HZ, candidate_mono.len());
    assert!(
        tone_dominates(&wrong_pitch, PITCH_SHIFTED_TONE_HZ, TONE_HZ),
        "wrong-pitch negative control does not contain its declared frequency"
    );
    assert!(
        !tone_dominates(&wrong_pitch, TONE_HZ, PITCH_SHIFTED_TONE_HZ),
        "pitch oracle accepted a deliberately wrong 352 Hz fixture as 440 Hz"
    );

    assert_oracle_load_bearing(&control.pcm, CHANNELS, SAMPLE_RATE, BLOCK_FRAMES);
    assert_oracle_load_bearing(&candidate.pcm, CHANNELS, SAMPLE_RATE, BLOCK_FRAMES);
    assert_frame_oracle_load_bearing(&candidate.pcm);
    if record_artifacts {
        let backend_label = backend.to_string().to_ascii_lowercase();
        let artifact_case = format!("no-sync-active-keylock-{backend_label}");
        let manifest = ActiveStretchManifest {
            case: "no-sync-active-keylock",
            backend: &backend_label,
            speed: ACTIVE_SPEED,
            sample_rate: SAMPLE_RATE,
            channels: CHANNELS,
            control: CaptureMetrics::from(&control),
            candidate: CaptureMetrics::from(&candidate),
            control_cochlea: &control_report,
            candidate_cochlea: &candidate_report,
            candidate_sine_fit: &candidate_sine_fit,
            candidate_continuity: &candidate_continuity,
            rate_control: &rate_control_timing,
            rate_candidate: &rate_candidate_timing,
            failures: &failures,
        };
        let written = write_audio_artifact(
            &artifact_case,
            SAMPLE_RATE,
            CHANNELS,
            &[
                ("effect-free-control", &control.pcm),
                ("active", &candidate.pcm),
                ("rate-marker-control", &rate_control.pcm),
                ("rate-marker-active", &rate_candidate.pcm),
            ],
            &manifest,
        )
        .expect("active no-SYNC audio artifact write");
        assert!(
            written.is_some(),
            "KITHARA_AUDIO_ARTIFACT_DIR must be set for the artifact recorder"
        );
    }
    assert!(
        failures.is_empty(),
        "active no-SYNC stretch failed for {backend}: {}\ncontrol={control_report:?}\ncandidate={candidate_report:?}",
        failures.join("; "),
    );
}

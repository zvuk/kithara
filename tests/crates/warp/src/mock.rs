//! Offline warp playback fixtures.

#![cfg(not(target_arch = "wasm32"))]

use std::num::NonZeroU32;

use kithara::{
    audio::{AudioConfig, NoResamplerBackend},
    effects::AudioEffect,
    events::TrackId,
    host::{HostConfig, HostSettings},
    platform::{
        flash::real_io,
        sync::{
            Arc,
            atomic::{AtomicU8, Ordering},
        },
        time::{self, Duration, Instant},
    },
    play::TrackConfig,
    queue::{Queue, QueueConfig, QueueEvent, TrackStatus, Transition},
    signal::AudioChunk,
    warp::{StretchKind, WarpConfig},
};
#[cfg(not(target_os = "android"))]
use kithara_integration_tests::audio_artifact::write_audio_artifact;
use kithara_integration_tests::{
    bufpool_ext::pools,
    cochlea::{CochleaReport, assert_oracle_load_bearing, continuity_failures, percentile_f32},
    kithara,
    memory_source::{MemStream, MemStreamConfig, MemorySource},
    mock::InjectedFactory,
    offline::{OfflinePlayer, OfflinePlayerOptions},
    waits::wait_for_loader_done_event,
};
use kithara_test_fixtures::{asset::Asset, assets::sine_wav_a440_6s, signal::goertzel_magnitude};
use kithara_test_utils::kithara::hang_watchdog;
use num_traits::ToPrimitive;
#[cfg(not(target_os = "android"))]
use serde::Serialize;

use crate::consts::{
    BLOCK_FRAMES, CAPTURE_BLOCKS, CHANNELS, CONTINUITY_RATIO, CONTINUITY_SLACK, LOAD_BURST,
    LOAD_CAPTURE, LOAD_CAPTURE_SEEN, LOAD_DONE, LOAD_INTERVAL_BLOCKS, LOAD_WARMUP,
    MAX_ACTIVE_STEREO_DELTA, RATE_MARKER_JOIN_FRAMES, RATE_MARKER_START_FRAMES,
    RATE_TIMING_TOLERANCE_FRAMES, RATE_WINDOW_FRAMES, SAMPLE_RATE, TONE_DOMINANCE_RATIO, TONE_HZ,
    WARMUP_BLOCKS,
};
pub use crate::{
    active::run_active_stretch,
    fixtures::{marked_source_pcm, source_pcm},
};
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
    blocks: usize,
    probe: Arc<LoadProbe>,
}

impl BurstLoadEffect {
    fn new(probe: Arc<LoadProbe>) -> Self {
        Self { blocks: 0, probe }
    }
}

impl AudioEffect for BurstLoadEffect {
    fn flush(&mut self) -> Option<AudioChunk> {
        None
    }

    fn held_source_frames(&self) -> u64 {
        0
    }

    /// Pass the chunk through, burning real CPU on every `LOAD_INTERVAL_BLOCKS`th
    /// one.
    ///
    /// The real region is what makes the burst a burst. This runs on the
    /// producer thread, which the quiescence engine counts as running, so a
    /// virtual `Instant` would have the spin wait for a clock that cannot move
    /// until the spin ends. Carving the region REAL keeps the deadline on the
    /// host clock, so the contention the load player is here to create stays
    /// contention under flash as well as off it.
    #[kithara::flash(false)]
    #[hang_watchdog]
    fn process(&mut self, chunk: AudioChunk) -> Option<AudioChunk> {
        self.blocks = self.blocks.saturating_add(1);
        if self.blocks.is_multiple_of(LOAD_INTERVAL_BLOCKS) {
            self.probe.observe_burst();
            let deadline = Instant::now() + LOAD_BURST;
            while Instant::now() < deadline {
                hang_tick!();
                std::hint::spin_loop();
            }
            hang_reset!();
        }
        Some(chunk)
    }

    fn reset(&mut self) {
        self.blocks = 0;
    }
}

#[derive(Debug)]
pub(super) struct RealtimeCapture {
    pub(super) pcm: Vec<f32>,
    pub(super) warmup_decode_errors: u64,
    pub(super) warmup_underruns: u64,
    pub(super) decode_errors: u64,
    pub(super) underruns: u64,
    pub(super) load_observed_during_capture: bool,
}

#[cfg(not(target_os = "android"))]
#[derive(Serialize, kithara_derive::Mirror)]
#[mirror(from_ref = RealtimeCapture)]
pub(super) struct CaptureMetrics {
    warmup_decode_errors: u64,
    warmup_underruns: u64,
    decode_errors: u64,
    underruns: u64,
    load_observed_during_capture: bool,
}

#[derive(Debug, Serialize)]
pub(super) struct SineFit {
    pub(super) amplitude: f64,
    pub(super) dc: f64,
    pub(super) non_finite_samples: usize,
    pub(super) residual_ratio: f64,
    pub(super) stereo_mismatches: usize,
    pub(super) max_stereo_delta: f32,
}

#[derive(Debug, Serialize)]
pub(super) struct ChannelContinuity {
    channel: usize,
    peak_step: f32,
    background_step: f32,
    step_limit: f32,
    peak_residual: f32,
    background_residual: f32,
    residual_limit: f32,
}

#[derive(Debug, Serialize)]
pub(super) struct MarkerTiming {
    background_rms: f32,
    expected_interval_frames: usize,
    marker_frames: [usize; 2],
    marker_rms: f32,
    measured_interval_frames: usize,
}

#[cfg(not(target_os = "android"))]
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

pub(super) fn measure_quiet_sine(samples: &[f32]) -> SineFit {
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

/// The Warp configuration of a deck at unity, or at `speed` on a keylocked
/// `backend`.
fn warp_config(stretch: Option<(StretchKind, f32)>) -> WarpConfig {
    stretch.map_or_else(
        || WarpConfig::builder().build(),
        |(backend, speed)| {
            WarpConfig::builder()
                .speed(speed)
                .backend(backend)
                .keylock(true)
                .build()
        },
    )
}

fn audio_config(
    source: &[u8],
    warp: WarpConfig,
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
        .warp(warp)
        .effects(
            effects
                .into_iter()
                .map(kithara::effects::EffectSetup::Prepared)
                .collect(),
        )
        .build()
}

#[hang_watchdog]
async fn passthrough_player(
    asset: &Asset,
    warp: WarpConfig,
    effects: Vec<Box<dyn AudioEffect>>,
) -> OfflinePlayer<InjectedFactory> {
    let factory = InjectedFactory::default();
    let player = OfflinePlayer::with_factory(
        OfflinePlayerOptions::builder()
            .crossfade_duration(0.0)
            .warp(warp.clone())
            .build(),
        HostConfig::offline(pools())
            .settings(
                HostSettings::builder()
                    .sample_rate(NonZeroU32::new(SAMPLE_RATE).expect("sample rate is non-zero"))
                    .build(),
            )
            .build(),
        factory.clone(),
    )
    .await;
    let source = kithara_integration_tests::offline::asset_source(asset);
    let load = kithara::play::mock::track_load(
        audio_config(asset.bytes(), warp, effects),
        Arc::from(source.as_str()),
        player.worker().clone(),
        NonZeroU32::new(SAMPLE_RATE).expect("sample rate is non-zero"),
        |worker, config, position, start, inbox| {
            Box::pin(async move { worker.load(config, position, start, inbox).await })
        },
    );
    let mut events = player.player().subscribe();
    let id = TrackId::allocate();
    factory.insert(id, load);
    let id = player
        .run(player.player(), move |queue| {
            let id = queue
                .append_with_id(id, source)
                .expect("append explicit source fixture");
            queue
                .select(id, Transition::None)
                .expect("select explicit source fixture");
            id
        })
        .await;
    let mut budget = Duration::ZERO;
    hang_park!(|remaining: Duration| budget = remaining.min(Duration::from_secs(5)));
    let loaded = wait_for_loader_done_event(&mut events, player.player(), id, budget).await;
    hang_tick!();
    loaded.expect("audio preload gate must open");
    hang_reset!();
    player
        .run(player.player(), kithara::queue::QueueControl::play)
        .await;
    player
}

/// Render the source at the real device cadence and capture the result.
///
/// The guard puts the pacing sleep on the same clock as the decode worker's
/// park: the worker is a registered pacer, so a block period elapses only once
/// it has produced. Without it the sleep is a real `tokio` timer — the test
/// macro rewrites time calls in the test body, not in the helpers it calls —
/// and the consumer would advance at host speed against a producer advancing at
/// virtual speed, draining the ring into zeros and making every PCM oracle a
/// property of the machine. The burst load stays REAL: `BurstLoadEffect` spins
/// on the producer thread, whose callstack never enters this guard.
#[kithara::flash(true)]
#[hang_watchdog]
pub(super) async fn render_passthrough(
    asset: &Asset,
    stretch: Option<(StretchKind, f32)>,
    with_load: bool,
) -> RealtimeCapture {
    let load_probe = Arc::new(LoadProbe::new());
    let target = passthrough_player(asset, warp_config(stretch), Vec::new()).await;
    let mut load = if with_load {
        Some(
            passthrough_player(
                asset,
                warp_config(None),
                vec![Box::new(BurstLoadEffect::new(Arc::clone(&load_probe)))],
            )
            .await,
        )
    } else {
        None
    };
    let block_period = Duration::from_secs_f64(
        f64::from(u32::try_from(BLOCK_FRAMES).expect("block size fits u32"))
            / f64::from(SAMPLE_RATE),
    );

    let metrics_before_warmup = target.metrics();
    for block in 0..WARMUP_BLOCKS {
        hang_tick!();
        let started = Instant::now();
        if let Some(player) = load.as_mut() {
            let _ = player.render(BLOCK_FRAMES).await;
        }
        let _ = target.render(BLOCK_FRAMES).await;
        let metrics = target.metrics();
        assert_eq!(
            metrics.underruns(),
            metrics_before_warmup.underruns(),
            "no-SYNC warmup underrun: stretch={stretch:?}, with_load={with_load}, \
             block={block}, output_frames={}..{}, before={metrics_before_warmup:?}, \
             now={metrics:?}",
            block * BLOCK_FRAMES,
            (block + 1) * BLOCK_FRAMES,
        );
        time::sleep(block_period.saturating_sub(started.elapsed())).await;
        hang_reset!();
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
    for block in 0..CAPTURE_BLOCKS {
        hang_tick!();
        let started = Instant::now();
        if let Some(player) = load.as_mut() {
            let _ = player.render(BLOCK_FRAMES).await;
        }
        pcm.extend(target.render(BLOCK_FRAMES).await);
        let metrics = target.metrics();
        assert_eq!(
            metrics.underruns(),
            metrics_before_capture.underruns(),
            "no-SYNC capture underrun: stretch={stretch:?}, with_load={with_load}, \
             block={block}, output_frames={}..{}, before={metrics_before_capture:?}, \
             now={metrics:?}",
            (WARMUP_BLOCKS + block) * BLOCK_FRAMES,
            (WARMUP_BLOCKS + block + 1) * BLOCK_FRAMES,
        );
        time::sleep(block_period.saturating_sub(started.elapsed())).await;
        hang_reset!();
    }
    let load_observed_during_capture = load_probe.finish_capture();
    let metrics_after_capture = target.metrics();

    let capture = RealtimeCapture {
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
    };
    if let Some(player) = load {
        let mut budget = Duration::ZERO;
        hang_park!(|remaining| budget = remaining);
        time::timeout(budget, player.close())
            .await
            .expect("load player shutdown watchdog");
        hang_tick!();
        hang_reset!();
    }
    let mut budget = Duration::ZERO;
    hang_park!(|remaining| budget = remaining);
    time::timeout(budget, target.close())
        .await
        .expect("player shutdown watchdog");
    hang_tick!();
    capture
}

/// Render the queue fixture through a `Queue` control at the same cadence.
///
/// Carries the clock guard of [`render_passthrough`] for the same reason: the
/// tick-and-render pair must not outrun the decode worker.
#[kithara::flash(true)]
#[hang_watchdog]
async fn render_queue_passthrough(stretch: Option<(StretchKind, f32)>) -> Vec<f32> {
    let harness = OfflinePlayer::with_sample_rate(
        OfflinePlayerOptions::builder()
            .crossfade_duration(0.0)
            .warp(warp_config(stretch))
            .build(),
        SAMPLE_RATE,
    )
    .await;
    let queue = harness
        .insert_control(Queue::new(
            QueueConfig::builder()
                .prep(harness.resource_prep().clone())
                .build(),
        ))
        .await;
    let mut events = queue.subscribe::<QueueEvent>();
    let source = sine_wav_a440_6s()
        .path()
        .expect("the queue fixture lives on disk")
        .to_string_lossy()
        .into_owned();
    let id = {
        // WHY: The product loader reads this fixture off disk and the engine does not count that read, so a bare virtual deadline here is
        // spent at the first quiescence rather than on the load: five seconds collapse into microseconds and the wait reports a load that
        // is still in flight. The real-I/O bracket paces the clock to host time for this region alone, giving the load the real budget it
        // had before the render loops went virtual, while those loops keep collapsing.
        let _real_io = real_io();
        let id = harness
            .run(&queue, move |q| q.append(source))
            .await
            .expect("append local queue fixture");
        assert!(
            async {
                loop {
                    hang_tick!();
                    let mut budget = Duration::ZERO;
                    hang_park!(|remaining: Duration| budget = remaining.min(Duration::from_secs(5)));
                    let event = time::timeout(budget, events.recv()).await;
                    hang_tick!();
                    let Ok(Ok(envelope)) = event else {
                        break;
                    };
                    if matches!(
                        envelope.event,
                        QueueEvent::TrackStatusChanged {
                            id: seen,
                            status: TrackStatus::Loaded,
                        } if seen == id
                    ) {
                        hang_reset!();
                        return true;
                    }
                }
                false
            }
            .await,
            "local queue fixture must load through the product loader"
        );
        id
    };
    harness
        .run(&queue, move |q| q.select(id, Transition::None))
        .await
        .expect("select queue passthrough track");

    let block_period = Duration::from_secs_f64(
        f64::from(u32::try_from(BLOCK_FRAMES).expect("block size fits u32"))
            / f64::from(SAMPLE_RATE),
    );
    for _ in 0..WARMUP_BLOCKS {
        hang_tick!();
        let started = Instant::now();
        harness
            .run(&queue, kithara::queue::QueueControl::tick)
            .await
            .expect("tick queue during warmup");
        let _ = harness.render(BLOCK_FRAMES).await;
        time::sleep(block_period.saturating_sub(started.elapsed())).await;
        hang_reset!();
    }

    let mut pcm = Vec::with_capacity(CAPTURE_BLOCKS * BLOCK_FRAMES * usize::from(CHANNELS));
    for _ in 0..CAPTURE_BLOCKS {
        hang_tick!();
        let started = Instant::now();
        harness
            .run(&queue, kithara::queue::QueueControl::tick)
            .await
            .expect("tick queue during capture");
        pcm.extend(harness.render(BLOCK_FRAMES).await);
        time::sleep(block_period.saturating_sub(started.elapsed())).await;
        hang_reset!();
    }
    drop(queue);
    let mut budget = Duration::ZERO;
    hang_park!(|remaining| budget = remaining);
    time::timeout(budget, harness.close())
        .await
        .expect("queue harness shutdown watchdog");
    hang_tick!();
    pcm
}

pub(super) fn first_sample_mismatch(candidate: &[f32], control: &[f32]) -> Option<usize> {
    candidate
        .iter()
        .zip(control)
        .position(|(candidate, control)| candidate.to_bits() != control.to_bits())
        .or_else(|| (candidate.len() != control.len()).then(|| candidate.len().min(control.len())))
}

pub(super) fn first_channel(samples: &[f32]) -> Vec<f32> {
    samples
        .chunks_exact(usize::from(CHANNELS))
        .map(|frame| frame[0])
        .collect()
}

pub(super) fn tone_dominates(samples: &[f32], tone_hz: f64, competing_hz: f64) -> bool {
    let tone = goertzel_magnitude(samples, tone_hz, SAMPLE_RATE);
    let competing = goertzel_magnitude(samples, competing_hz, SAMPLE_RATE);
    tone > competing * TONE_DOMINANCE_RATIO
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

pub(super) fn marker_timing(samples: &[f32], speed: f32) -> MarkerTiming {
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

pub(super) fn marker_timing_failures(label: &str, timing: &MarkerTiming) -> Vec<String> {
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

pub(super) fn frame_continuity(samples: &[f32]) -> (Vec<ChannelContinuity>, Vec<String>) {
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

pub(super) fn assert_frame_oracle_load_bearing(control: &[f32]) {
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

/// Renders the tone with no SYNC at unity speed and checks it stays transparent.
///
/// # Panics
///
/// Panics when the fixture differs from `source` or the render is not transparent.
pub async fn run_no_sync_passthrough(
    source: &'static [u8],
    backend: StretchKind,
    record_artifacts: bool,
) {
    let channels = usize::from(CHANNELS);
    let asset = sine_wav_a440_6s();
    assert_eq!(asset.bytes(), source);
    let baseline = render_passthrough(&asset, None, false).await;
    let baseline_report = CochleaReport::measure(&baseline.pcm, CHANNELS, SAMPLE_RATE);
    let baseline_source_fit = measure_quiet_sine(&baseline.pcm);
    let unity = render_passthrough(&asset, Some((backend, 1.0)), false).await;
    let unity_report = CochleaReport::measure(&unity.pcm, CHANNELS, SAMPLE_RATE);
    let loaded = render_passthrough(&asset, Some((backend, 1.0)), true).await;
    let loaded_report = CochleaReport::measure(&loaded.pcm, CHANNELS, SAMPLE_RATE);
    let queue_baseline = render_queue_passthrough(None).await;
    let queue_unity = render_queue_passthrough(Some((backend, 1.0))).await;
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

    #[cfg(target_os = "android")]
    assert!(
        !record_artifacts,
        "listening artifact export belongs to the host suite"
    );
    #[cfg(not(target_os = "android"))]
    if record_artifacts {
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

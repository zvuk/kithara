#![cfg(not(target_arch = "wasm32"))]

use std::{
    num::{NonZeroU32, NonZeroUsize},
    path::Path,
};

use kithara::{
    host::HostOwned,
    platform::time::{self, Duration},
    play::{ResourceConfig, ResourceSrc},
    queue::{Queue, QueueConfig, Transition},
    stretch::{BungeeConfig, ElasticBackendConfig, SignalsmithConfig},
    warp::{StretchControls, StretchKind, WarpConfig},
};
use kithara_integration_tests::{
    TestTempDir, disk_asset_store, kithara,
    offline::{OfflinePlayerHarness, OfflinePlayerOptions},
    temp_dir,
    waits::wait_for_loader_done_event,
};
use kithara_test_fixtures::{assets::signal_mp3_sine880_30s, signal::goertzel_magnitude};

#[kithara::fixture]
fn response_source() -> &'static Path {
    signal_mp3_sine880_30s()
        .path()
        .expect("generated sine fixture is stored on disk")
}
use kithara_test_utils::probe::capture::{self as probe_capture, ProbeEvent, Recorder};

use crate::bufpool_ext::TestPools;

const SAMPLE_RATE: u32 = 44_100;
const CHANNELS: u16 = 2;
const TARGET_WINDOW_FRAMES: usize = 128;
const TONES_HZ: [f64; 4] = [440.0, 880.0, 1_760.0, 3_520.0];
const TONE_DOMINANCE_RATIO: f64 = 4.0;
const MIN_SIGNAL_RMS: f64 = 0.003;
const WARMUP_BLOCK_BUDGET: usize = 200;

fn response_backends() -> ElasticBackendConfig {
    ElasticBackendConfig::builder()
        .signalsmith(
            SignalsmithConfig::builder()
                .block_frames(NonZeroUsize::new(224).expect("case block is non-zero"))
                .interval_frames(NonZeroUsize::new(32).expect("case interval is non-zero"))
                .build(),
        )
        .bungee(
            BungeeConfig::builder()
                .log2_synthesis_hop_adjust(-4)
                .build(),
        )
        .build()
}

const MINIMUM: ResponseCase = ResponseCase::new(128, 4_096, 16, 1, 441, 1.0, 1, 2.0, 2, 0);
const PRODUCT: ResponseCase = ResponseCase::new(128, 8_192, 32, 12, 441, 2.0, 2, 0.5, 0, 0);
const EXTREME: ResponseCase = ResponseCase::new(64, 16_384, 32, 64, 441, 0.5, 0, 4.0, 3, 64);

#[derive(Clone, Copy, Debug)]
struct ResponseCase {
    callback_frames: usize,
    source_block_frames: usize,
    render_quantum_frames: usize,
    smooth_frames: usize,
    response_budget_frames: usize,
    initial_rate: f32,
    initial_tone: usize,
    target_rate: f32,
    target_tone: usize,
    burst: usize,
}

impl ResponseCase {
    const fn new(
        callback_frames: usize,
        source_block_frames: usize,
        render_quantum_frames: usize,
        smooth_frames: usize,
        response_budget_frames: usize,
        initial_rate: f32,
        initial_tone: usize,
        target_rate: f32,
        target_tone: usize,
        burst: usize,
    ) -> Self {
        Self {
            callback_frames,
            source_block_frames,
            render_quantum_frames,
            smooth_frames,
            response_budget_frames,
            initial_rate,
            initial_tone,
            target_rate,
            target_tone,
            burst,
        }
    }

    fn observation_frames(self) -> usize {
        self.response_budget_frames
            .saturating_add(self.source_block_frames.saturating_mul(2))
            .saturating_add(TARGET_WINDOW_FRAMES)
            .saturating_add(self.callback_frames.saturating_mul(2))
    }
}

fn frame_period(frames: usize) -> Duration {
    Duration::from_secs_f64(
        f64::from(u32::try_from(frames).expect("callback frame count fits u32"))
            / f64::from(SAMPLE_RATE),
    )
}

fn signal_rms(samples: &[f32]) -> f64 {
    let mut energy = 0.0;
    let mut frames = 0_u32;
    for frame in samples.chunks_exact(usize::from(CHANNELS)) {
        energy = f64::from(frame[0]).mul_add(f64::from(frame[0]), energy);
        frames += 1;
    }
    if frames == 0 {
        return 0.0;
    }
    (energy / f64::from(frames)).sqrt()
}

fn tone_is_dominant(samples: &[f32], target: usize) -> bool {
    let mut channel = [0.0; TARGET_WINDOW_FRAMES];
    let mut frames = 0;
    for (sample, frame) in channel
        .iter_mut()
        .zip(samples.chunks_exact(usize::from(CHANNELS)))
    {
        *sample = frame[0];
        frames += 1;
    }
    let magnitudes = TONES_HZ.map(|tone| goertzel_magnitude(&channel[..frames], tone, SAMPLE_RATE));
    signal_rms(samples) >= MIN_SIGNAL_RMS
        && magnitudes.iter().enumerate().all(|(index, magnitude)| {
            index == target || magnitudes[target] > magnitude * TONE_DOMINANCE_RATIO
        })
}

fn first_target_onset(samples: &[f32], command_frame: usize, target: usize) -> Option<usize> {
    let channels = usize::from(CHANNELS);
    let frames = samples.len() / channels;
    (command_frame + TARGET_WINDOW_FRAMES..=frames).find_map(|end| {
        let start = end - TARGET_WINDOW_FRAMES;
        tone_is_dominant(&samples[start * channels..end * channels], target)
            .then_some(start - command_frame)
    })
}

async fn capture_frames(
    harness: &OfflinePlayerHarness,
    frames: usize,
    callback_frames: usize,
) -> Vec<f32> {
    let mut samples = Vec::with_capacity(frames * usize::from(CHANNELS));
    while samples.len() / usize::from(CHANNELS) < frames {
        let remaining = frames - samples.len() / usize::from(CHANNELS);
        let block_frames = remaining.min(callback_frames);
        samples.extend(harness.render(block_frames).await);
        let _ = harness.tick_and_drain().await;
        time::sleep(frame_period(block_frames)).await;
    }
    samples
}

async fn capture_command_boundary(
    harness: &OfflinePlayerHarness,
    recorder: &Recorder,
    target: usize,
    callback_frames: usize,
) -> Vec<f32> {
    let mut publish_seq = latest_probe_seq(&recorder.snapshot(), "publish");
    let mut samples = Vec::new();
    for _ in 0..WARMUP_BLOCK_BUDGET {
        let block = capture_frames(harness, callback_frames, callback_frames).await;
        samples.extend_from_slice(&block);
        let after = recorder.snapshot();
        let current_publish_seq = latest_probe_seq(&after, "publish");
        let published = current_publish_seq > publish_seq;
        publish_seq = current_publish_seq;
        let frames = samples.len() / usize::from(CHANNELS);
        if published
            && frames >= TARGET_WINDOW_FRAMES
            && tone_is_dominant(
                &samples[(frames - TARGET_WINDOW_FRAMES) * usize::from(CHANNELS)..],
                target,
            )
        {
            return samples;
        }
    }
    let events = recorder.snapshot();
    let publish = events
        .iter()
        .filter(|event| event.probe_name() == Some("publish"))
        .count();
    let rendered = events
        .iter()
        .filter(|event| event.probe_name() == Some("render_committed"))
        .count();
    let consumed = events
        .iter()
        .filter(|event| event.probe_name() == Some("pcm_consumed"))
        .count();
    let applied = events
        .iter()
        .filter(|event| event.probe_name() == Some("rate_applied"))
        .count();
    panic!(
        "precondition: no callback presented the initial tone at a transport boundary; publish={publish}, rendered={rendered}, rate_applied={applied}, pcm_consumed={consumed}, last_rms={}",
        signal_rms(&samples)
    );
}

fn latest_probe_seq(events: &[ProbeEvent], name: &str) -> Option<u64> {
    events
        .iter()
        .filter(|event| event.probe_name() == Some(name))
        .filter_map(ProbeEvent::seq)
        .max()
}

async fn playing_queue(
    temp_dir: &TestTempDir,
    backend: StretchKind,
    backends: ElasticBackendConfig,
    case: ResponseCase,
    response_source: &'static Path,
) -> (OfflinePlayerHarness, HostOwned<Queue<TestPools>>) {
    let stretch = StretchControls::new(1.0);
    stretch.set_backend(backend);
    let warp = WarpConfig::builder()
        .stretch(stretch)
        .backends(backends)
        .source_block_frames(
            NonZeroUsize::new(case.source_block_frames).expect("case source block is non-zero"),
        )
        .rate_smooth_frames(
            NonZeroUsize::new(case.smooth_frames).expect("case smoothing is non-zero"),
        )
        .render_quantum_frames(
            NonZeroUsize::new(case.render_quantum_frames).expect("case quantum is non-zero"),
        )
        .build();
    let harness = OfflinePlayerHarness::with_sample_rate(
        OfflinePlayerOptions::builder()
            .crossfade_duration(0.0)
            .warp(warp)
            .output_block_frames(
                NonZeroU32::new(
                    u32::try_from(case.callback_frames).expect("case callback fits u32"),
                )
                .expect("case callback is non-zero"),
            )
            .response_budget_frames(
                NonZeroUsize::new(case.response_budget_frames)
                    .expect("case response budget is non-zero"),
            )
            .build(),
        SAMPLE_RATE,
    )
    .await;
    let queue = Queue::new(
        QueueConfig::builder()
            .player(harness.take_player())
            .should_autoplay(false)
            .build(),
    );
    let queue = harness.insert(queue).await;
    queue.set_default_rate(case.initial_rate);
    let path = response_source;
    let config: ResourceConfig<TestPools> = ResourceConfig::for_src(
        ResourceSrc::parse(path.to_str().expect("utf-8 fixture path"))
            .expect("fixture path is a valid resource source"),
    )
    .store(disk_asset_store(
        temp_dir.path().join("rate-response-store"),
    ))
    .build();
    let mut events = queue.subscribe();
    let id = harness
        .run(queue.control(), move |q| q.append(config))
        .await
        .expect("append sine fixture");
    wait_for_loader_done_event(&mut events, &queue, id, Duration::from_secs(30))
        .await
        .expect("load sine fixture through resident queue");
    harness
        .run(queue.control(), move |q| q.select(id, Transition::None))
        .await
        .expect("select live-rate fixture");
    (harness, queue)
}

fn revision_probe<'a>(
    events: &'a [ProbeEvent],
    name: &str,
    field: &str,
    revision: u64,
) -> Option<&'a ProbeEvent> {
    events
        .iter()
        .filter(|event| event.probe_name() == Some(name))
        .filter(|event| event.u64(field) == Some(revision))
        .min_by_key(|event| event.seq().unwrap_or(u64::MAX))
}

fn assert_response(
    backend: StretchKind,
    case: ResponseCase,
    command_frame: usize,
    samples: &[f32],
    events: &[ProbeEvent],
) {
    let requested = events
        .iter()
        .filter(|event| event.probe_name() == Some("rate_requested"))
        .max_by_key(|event| event.seq().unwrap_or(0))
        .unwrap_or_else(|| {
            let publish = events
                .iter()
                .filter(|event| event.probe_name() == Some("publish"))
                .count();
            let consumed = events
                .iter()
                .filter(|event| event.probe_name() == Some("pcm_consumed"))
                .count();
            let applied = events
                .iter()
                .filter(|event| event.probe_name() == Some("rate_applied"))
                .count();
            panic!(
                "{backend} emitted no rate_requested probe; publish={publish}, rate_applied={applied}, pcm_consumed={consumed}"
            )
        });
    let revision = requested
        .u64("request_revision")
        .unwrap_or_else(|| panic!("{backend} request probe has no revision"));
    assert_eq!(
        requested.u64("target_rate_bits"),
        Some(u64::from(case.target_rate.to_bits())),
        "{backend} correlated the wrong final rate request"
    );
    let request_frame = requested
        .u64("session_frame")
        .and_then(|frame| i64::try_from(frame).ok())
        .unwrap_or_else(|| panic!("{backend} request probe has no session frame"));
    let budget = i64::try_from(case.response_budget_frames).expect("case budget fits i64");
    let observed_frames = samples
        .len()
        .checked_div(usize::from(CHANNELS))
        .and_then(|frames| frames.checked_sub(command_frame))
        .expect("captured output includes the command boundary");
    let applied = revision_probe(events, "rate_applied", "request_revision", revision)
        .unwrap_or_else(|| {
            let applied: Vec<_> = events
                .iter()
                .filter(|event| event.probe_name() == Some("rate_applied"))
                .map(|event| {
                    (
                        event.u64("request_revision"),
                        event.u64("session_frame"),
                        event
                            .u64("applied_rate_bits")
                            .and_then(|bits| u32::try_from(bits).ok())
                            .map(f32::from_bits),
                        event.u64("source_start"),
                        event.u64("source_end"),
                    )
                })
                .collect();
            let presented: Vec<_> = events
                .iter()
                .filter(|event| event.probe_name() == Some("pcm_consumed"))
                .filter_map(|event| {
                    event
                        .u64("render_revision")
                        .zip(event.u64("output_start"))
                        .zip(event.u64("output_end"))
                })
                .collect();
            let rendered: Vec<_> = events
                .iter()
                .filter(|event| event.probe_name() == Some("render_committed"))
                .filter_map(|event| {
                    event
                        .u64("source_start")
                        .zip(event.u64("source_end"))
                        .zip(event.u64("output_start"))
                        .zip(event.u64("output_end"))
                })
                .collect();
            let published: Vec<_> = events
                .iter()
                .filter(|event| event.probe_name() == Some("publish"))
                .filter_map(|event| {
                    event
                        .u64("source")
                        .zip(event.u64("presentation_frame"))
                        .zip(event.u64("output_start"))
                        .zip(event.u64("output_end"))
                })
                .collect();
            let target_onset = first_target_onset(samples, command_frame, case.target_tone);
            panic!(
                "{backend} did not apply revision {revision} within {observed_frames} rendered output frames; response budget is {budget}; target_onset={target_onset:?}; applied={applied:?}; presented={presented:?}; rendered={rendered:?}; published={published:?}"
            )
        });
    let consumed = revision_probe(events, "pcm_consumed", "render_revision", revision)
        .unwrap_or_else(|| panic!("{backend} presented no PCM for {revision}"));
    let applied_frame = applied
        .u64("session_frame")
        .and_then(|frame| i64::try_from(frame).ok())
        .unwrap_or_else(|| panic!("{backend} apply probe has no session frame"));
    let applied_rate = applied
        .u64("applied_rate_bits")
        .and_then(|bits| u32::try_from(bits).ok())
        .map(f32::from_bits)
        .unwrap_or_else(|| panic!("{backend} apply probe has no rate"));
    let consumed_frame = consumed
        .u64("output_start")
        .and_then(|frame| i64::try_from(frame).ok())
        .unwrap_or_else(|| panic!("{backend} PCM probe has no output start"));
    let applied_response = applied_frame
        .checked_sub(request_frame)
        .unwrap_or_else(|| panic!("{backend} applied revision before its request"));
    let presented_response = consumed_frame
        .checked_sub(request_frame)
        .unwrap_or_else(|| panic!("{backend} presented revision before its request"));
    assert!(
        applied_response <= budget,
        "{backend} applied revision {revision} after {applied_response} frames; budget is {budget}"
    );
    assert!(
        presented_response <= budget,
        "{backend} presented revision {revision} after {presented_response} frames; applied after {applied_response} frames; apply-to-presentation delay is {} frames; budget is {budget}",
        presented_response - applied_response
    );
    if case.smooth_frames > 1 {
        let low = case.initial_rate.min(case.target_rate);
        let high = case.initial_rate.max(case.target_rate);
        assert!(
            applied_rate > low && applied_rate < high,
            "{backend} jumped from {} directly to {} instead of smoothing over {} output frames",
            case.initial_rate,
            applied_rate,
            case.smooth_frames
        );
    }
    let onset = first_target_onset(samples, command_frame, case.target_tone)
        .unwrap_or_else(|| panic!("{backend} never produced the target tone"));
    let primed = revision_probe(events, "prime_activation", "request_revision", revision)
        .map(|event| (event.u64("source_frames"), event.u64("output_frames")));
    assert!(
        onset <= case.response_budget_frames,
        "{backend} target tone began after {onset} frames; applied after {applied_response}; presented after {presented_response}; budget is {}; primed={primed:?}",
        case.response_budget_frames
    );
}

async fn run_case(
    temp_dir: &TestTempDir,
    backend: StretchKind,
    backends: ElasticBackendConfig,
    case: ResponseCase,
    response_source: &'static Path,
) {
    let (harness, queue) = playing_queue(temp_dir, backend, backends, case, response_source).await;
    let recorder = probe_capture::install();
    let mut samples =
        capture_command_boundary(&harness, &recorder, case.initial_tone, case.callback_frames)
            .await;
    let command_frame = samples.len() / usize::from(CHANNELS);
    assert!(
        queue.is_playing(),
        "{backend} command boundary is not playing"
    );
    let ready = recorder.snapshot();
    let published_end = ready
        .iter()
        .filter(|event| event.probe_name() == Some("publish"))
        .max_by_key(|event| event.seq().unwrap_or(0))
        .and_then(|event| event.u64("output_end"));
    let consumed_end = ready
        .iter()
        .filter(|event| event.probe_name() == Some("pcm_consumed"))
        .max_by_key(|event| event.seq().unwrap_or(0))
        .and_then(|event| event.u64("output_end"));
    let published_end = published_end
        .unwrap_or_else(|| panic!("{backend} command boundary has no published transport"));
    let consumed_end = consumed_end
        .unwrap_or_else(|| panic!("{backend} command boundary has no presented transport"));
    assert!(
        consumed_end <= published_end,
        "{backend} presented transport {consumed_end} is ahead of published transport {published_end}"
    );
    for command in 0..case.burst {
        queue.set_rate(if command.is_multiple_of(2) { 4.0 } else { 0.5 });
    }
    queue.set_rate(case.target_rate);
    samples.extend(capture_frames(&harness, case.observation_frames(), case.callback_frames).await);
    let events = recorder.snapshot();

    let precommand_start = command_frame.saturating_sub(TARGET_WINDOW_FRAMES);
    let precommand =
        &samples[precommand_start * usize::from(CHANNELS)..command_frame * usize::from(CHANNELS)];
    assert!(
        tone_is_dominant(precommand, case.initial_tone),
        "{backend} was not playing the initial tone before set_rate"
    );
    assert!(
        !tone_is_dominant(precommand, case.target_tone),
        "{backend} already contained the target tone before set_rate"
    );
    assert_response(backend, case, command_frame, &samples, &events);
    drop(queue);
    harness.close().await;
}

#[kithara::test(
    tokio,
    multi_thread,
    serial,
    flash(false),
    timeout(Duration::from_secs(60)),
    hang_timeout_secs(5)
)]
#[case::signalsmith_minimum(StretchKind::Signalsmith, response_backends(), MINIMUM)]
#[case::signalsmith_product(StretchKind::Signalsmith, response_backends(), PRODUCT)]
#[case::signalsmith_extreme(StretchKind::Signalsmith, response_backends(), EXTREME)]
#[cfg_attr(
    not(all(target_os = "windows", target_env = "msvc")),
    case::bungee_minimum(StretchKind::Bungee, response_backends(), MINIMUM)
)]
#[cfg_attr(
    not(all(target_os = "windows", target_env = "msvc")),
    case::bungee_product(StretchKind::Bungee, response_backends(), PRODUCT)
)]
#[cfg_attr(
    not(all(target_os = "windows", target_env = "msvc")),
    case::bungee_extreme(StretchKind::Bungee, response_backends(), EXTREME)
)]
async fn live_rate_change_reaches_presented_pcm_within_response_budget(
    temp_dir: TestTempDir,
    response_source: &'static Path,
    #[case] backend: StretchKind,
    #[case] backends: ElasticBackendConfig,
    #[case] case: ResponseCase,
) {
    run_case(&temp_dir, backend, backends, case, response_source).await;
}

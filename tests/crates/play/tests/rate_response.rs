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

async fn capture_until_applied(
    harness: &OfflinePlayerHarness,
    recorder: &Recorder,
    case: ResponseCase,
) -> (Vec<f32>, u64, Vec<ProbeEvent>) {
    let mut samples = Vec::new();
    for _ in 0..WARMUP_BLOCK_BUDGET {
        samples.extend(capture_frames(harness, case.callback_frames, case.callback_frames).await);
        let events = recorder.snapshot();
        let Some(revision) = events
            .iter()
            .filter(|event| event.probe_name() == Some("rate_requested"))
            .max_by_key(|event| event.seq().unwrap_or(0))
            .and_then(|event| event.u64("request_revision"))
        else {
            continue;
        };
        if revision_probe(&events, "rate_applied", "request_revision", revision).is_some() {
            return (samples, revision, events);
        }
    }
    let events = recorder.snapshot();
    let requested: Vec<_> = events
        .iter()
        .filter(|event| event.probe_name() == Some("rate_requested"))
        .filter_map(|event| {
            event
                .u64("request_revision")
                .zip(event.u64("target_rate_bits"))
        })
        .collect();
    let applied: Vec<_> = events
        .iter()
        .filter(|event| event.probe_name() == Some("rate_applied"))
        .filter_map(|event| {
            event
                .u64("request_revision")
                .zip(event.u64("session_frame"))
        })
        .collect();
    let published = events
        .iter()
        .filter(|event| event.probe_name() == Some("publish"))
        .count();
    let consumed = events
        .iter()
        .filter(|event| event.probe_name() == Some("pcm_consumed"))
        .count();
    panic!(
        "the renderer never applied the requested rate; requested={requested:?}; applied={applied:?}; publish={published}; pcm_consumed={consumed}"
    );
}

fn latest_probe_seq(events: &[ProbeEvent], name: &str) -> Option<u64> {
    events
        .iter()
        .filter(|event| event.probe_name() == Some(name))
        .filter_map(ProbeEvent::seq)
        .max()
}

fn latest_probe_u64(events: &[ProbeEvent], name: &str, field: &str) -> Option<u64> {
    events
        .iter()
        .filter(|event| event.probe_name() == Some(name))
        .max_by_key(|event| event.seq().unwrap_or(0))
        .and_then(|event| event.u64(field))
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
            .block_on_underrun(true)
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

/// Output frames in `[from, to)` that no span covers.
fn unrendered_frames(spans: &[(u64, u64)], from: u64, to: u64) -> u64 {
    let mut ordered = spans.to_vec();
    ordered.sort_unstable();
    let mut cursor = from;
    let mut missing = 0;
    for (start, end) in ordered {
        if cursor >= to || start >= to {
            break;
        }
        if end <= cursor {
            continue;
        }
        missing += start.saturating_sub(cursor);
        cursor = end.min(to).max(cursor);
    }
    missing + to.saturating_sub(cursor)
}

/// The output ranges the feeder filled from rendered source.
///
/// A read that outruns the producer is zero-filled rather than refused, and a
/// zero-filled range carries no span: the transport advances over frames the
/// renderer never produced. An onset search that crosses them measures how
/// fast the host decoded, not how fast the engine answered the rate.
fn consumed_spans(events: &[ProbeEvent]) -> Vec<(u64, u64)> {
    events
        .iter()
        .filter(|event| event.probe_name() == Some("pcm_consumed"))
        .filter_map(|event| Some((event.u64("output_start")?, event.u64("output_end")?)))
        .collect()
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
    apply_frame: usize,
    revision: u64,
    samples: &[f32],
    events: &[ProbeEvent],
    at_apply: &[ProbeEvent],
) {
    let requested = revision_probe(events, "rate_requested", "request_revision", revision)
        .unwrap_or_else(|| panic!("{backend} lost the request probe for {revision}"));
    assert_eq!(
        requested.u64("target_rate_bits"),
        Some(u64::from(case.target_rate.to_bits())),
        "{backend} correlated the wrong final rate request"
    );
    let applied = revision_probe(events, "rate_applied", "request_revision", revision)
        .unwrap_or_else(|| panic!("{backend} lost the apply probe for {revision}"));
    let new_rate_start = revision_probe(events, "pcm_consumed", "render_revision", revision)
        .and_then(|event| event.u64("output_start"))
        .unwrap_or_else(|| panic!("{backend} presented no PCM for {revision}"));
    let consumed_at_apply = latest_probe_u64(at_apply, "pcm_consumed", "output_end")
        .unwrap_or_else(|| panic!("{backend} apply boundary has no presented transport"));
    let queued = usize::try_from(new_rate_start.saturating_sub(consumed_at_apply))
        .expect("queued output frames fit usize");
    let applied_rate = applied
        .u64("applied_rate_bits")
        .and_then(|bits| u32::try_from(bits).ok())
        .map(f32::from_bits)
        .unwrap_or_else(|| panic!("{backend} apply probe has no rate"));
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
    let audible = (onset + command_frame).saturating_sub(apply_frame);
    let responded = audible.saturating_sub(queued);
    println!(
        "rate response: {backend} smooth={} audible={audible} queued={queued} \
         responded={responded} primed={primed:?}",
        case.smooth_frames
    );
    let observed_end = new_rate_start.saturating_add(
        u64::try_from(responded.saturating_add(TARGET_WINDOW_FRAMES))
            .expect("observed output frames fit u64"),
    );
    assert_eq!(
        unrendered_frames(&consumed_spans(events), consumed_at_apply, observed_end),
        0,
        "{backend} read output frames the renderer never produced between the apply boundary \
         at {consumed_at_apply} and the onset window ending at {observed_end}"
    );
    assert!(
        responded <= case.smooth_frames + TARGET_WINDOW_FRAMES,
        "{backend} took {responded} output frames to make revision {revision} audible once the \
         {queued} frames rendered at the old rate had drained; the ramp is {} frames and the \
         detector resolves an onset no finer than {TARGET_WINDOW_FRAMES}; audible={audible} \
         primed={primed:?}",
        case.smooth_frames
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
    let published_end = latest_probe_u64(&ready, "publish", "output_end");
    let consumed_end = latest_probe_u64(&ready, "pcm_consumed", "output_end");
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
    let (acknowledged, revision, at_apply) = capture_until_applied(&harness, &recorder, case).await;
    let apply_frame = command_frame + acknowledged.len() / usize::from(CHANNELS);
    samples.extend(acknowledged);
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
    assert_response(
        backend,
        case,
        command_frame,
        apply_frame,
        revision,
        &samples,
        &events,
        &at_apply,
    );
    drop(queue);
    harness.close().await;
}

/// The gap arithmetic must name every frame no span covers and must invent
/// none where consecutive spans meet, or the precondition it backs would
/// either pass over zero-fill or reject a continuous capture.
#[kithara::test]
fn unrendered_frames_counts_exactly_the_output_no_span_covers() {
    assert_eq!(unrendered_frames(&[(0, 32), (32, 64)], 0, 64), 0);
    assert_eq!(unrendered_frames(&[(0, 32), (64, 96)], 0, 96), 32);
    assert_eq!(unrendered_frames(&[(32, 64)], 0, 96), 64);
    assert_eq!(unrendered_frames(&[], 10, 20), 10);
    assert_eq!(unrendered_frames(&[(0, 200)], 10, 20), 0);
}

/// A live rate change becomes audible as soon as the PCM already rendered at the
/// old rate has drained.
///
/// The distance from the request to the apply is not a latency: the request runs
/// on the queue's owner thread and its probe samples a published render
/// snapshot, so the frame difference between the two reads two asynchronously
/// sampled counters. Neither is that backlog a constant - it is however far the
/// renderer had run ahead of the sink when the revision landed, and a loaded
/// machine lets it run further. So the run measures the backlog at the apply
/// boundary and holds the engine only to what it owns once that backlog is
/// spent: the parameter ramp, and the window the tone detector needs to call the
/// new rate dominant.
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
async fn a_live_rate_change_becomes_audible_within_the_pcm_already_rendered(
    temp_dir: TestTempDir,
    response_source: &'static Path,
    #[case] backend: StretchKind,
    #[case] backends: ElasticBackendConfig,
    #[case] case: ResponseCase,
) {
    run_case(&temp_dir, backend, backends, case, response_source).await;
}

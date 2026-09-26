#![cfg(not(target_arch = "wasm32"))]

use std::{
    num::{NonZeroU32, NonZeroUsize},
    path::PathBuf,
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
    audio_artifact::write_audio_artifact,
    disk_asset_store, kithara,
    offline::{OfflinePlayer, OfflinePlayerOptions},
    usdt_trace::{self, ProbeEvent, Scope},
    waits::wait_for_loader_done_event,
};
use kithara_test_fixtures::{assets::signal_mp3_sine880_30s, signal::goertzel_magnitude};
use kithara_test_utils::{TestTempDir, temp_dir};

#[kithara::fixture]
fn response_source() -> PathBuf {
    signal_mp3_sine880_30s()
        .path()
        .expect("generated sine fixture is stored on disk")
}

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

const RAMP: ResponseCase = ResponseCase::new(128, 8_192, 32, 882, 441, 2.0, 2, 4.0, 3, 0);

#[derive(Clone, Copy, Debug)]
struct ResponseCase {
    callback_frames: usize,
    source_block_frames: usize,
    render_quantum_frames: Option<NonZeroUsize>,
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
            render_quantum_frames: NonZeroUsize::new(render_quantum_frames),
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
    harness: &OfflinePlayer,
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
    harness: &OfflinePlayer,
    trace: &Scope,
    target: usize,
    callback_frames: usize,
) -> Vec<f32> {
    let mut publish_count = probe_count(&response_events(trace), "publish");
    let mut samples = Vec::new();
    for _ in 0..WARMUP_BLOCK_BUDGET {
        let block = capture_frames(harness, callback_frames, callback_frames).await;
        samples.extend_from_slice(&block);
        let after = response_events(trace);
        let current_publish_count = probe_count(&after, "publish");
        let published = current_publish_count > publish_count;
        publish_count = current_publish_count;
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
    let events = response_events(trace);
    let publish = events
        .iter()
        .filter(|event| event.probe == "publish")
        .count();
    let rendered = events
        .iter()
        .filter(|event| event.probe == "render_committed")
        .count();
    let consumed = events
        .iter()
        .filter(|event| event.probe == "pcm_consumed")
        .count();
    let applied = events
        .iter()
        .filter(|event| event.probe == "rate_applied")
        .count();
    panic!(
        "precondition: no callback presented the initial tone at a transport boundary; publish={publish}, rendered={rendered}, rate_applied={applied}, pcm_consumed={consumed}, last_rms={}",
        signal_rms(&samples)
    );
}

async fn capture_until_applied(
    harness: &OfflinePlayer,
    trace: &Scope,
    case: ResponseCase,
) -> (Vec<f32>, u64, Vec<ProbeEvent>) {
    let mut samples = Vec::new();
    for _ in 0..WARMUP_BLOCK_BUDGET {
        samples.extend(capture_frames(harness, case.callback_frames, case.callback_frames).await);
        let events = response_events(trace);
        let Some(revision) = events
            .iter()
            .rfind(|event| event.probe == "rate_requested")
            .and_then(|event| event.field("request_revision"))
        else {
            continue;
        };
        if revision_probe(&events, "rate_applied", "request_revision", revision).is_some() {
            return (samples, revision, events);
        }
    }
    let events = response_events(trace);
    let requested: Vec<_> = events
        .iter()
        .filter(|event| event.probe == "rate_requested")
        .filter_map(|event| {
            event
                .field("request_revision")
                .zip(event.field("target_rate_bits"))
        })
        .collect();
    let applied: Vec<_> = events
        .iter()
        .filter(|event| event.probe == "rate_applied")
        .filter_map(|event| {
            event
                .field("request_revision")
                .zip(event.field("session_frame"))
        })
        .collect();
    let published = probe_count(&events, "publish");
    let consumed = probe_count(&events, "pcm_consumed");
    panic!(
        "the renderer never applied the requested rate; requested={requested:?}; applied={applied:?}; publish={published}; pcm_consumed={consumed}"
    );
}

fn response_events(trace: &Scope) -> Vec<ProbeEvent> {
    [
        "publish",
        "rate_requested",
        "rate_applied",
        "pcm_consumed",
        "render_committed",
        "prime_activation",
        "chunk_admitted",
    ]
    .into_iter()
    .flat_map(|probe| trace.events_of(probe))
    .collect()
}

fn probe_count(events: &[ProbeEvent], name: &str) -> usize {
    events.iter().filter(|event| event.probe == name).count()
}

async fn playing_queue(
    temp_dir: &TestTempDir,
    backend: StretchKind,
    backends: ElasticBackendConfig,
    case: ResponseCase,
    response_source: PathBuf,
) -> (OfflinePlayer, HostOwned<Queue<TestPools>>) {
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
        .maybe_render_quantum_frames(case.render_quantum_frames)
        .build();
    let harness = OfflinePlayer::with_sample_rate(
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
    let queue = Queue::new(QueueConfig::builder().player(harness.take_player()).build());
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
        .filter(|event| event.probe == "pcm_consumed")
        .filter_map(|event| Some((event.field("output_start")?, event.field("output_end")?)))
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
        .find(|event| event.probe == name && event.field(field) == Some(revision))
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
        requested.field("target_rate_bits"),
        Some(u64::from(case.target_rate.to_bits())),
        "{backend} correlated the wrong final rate request"
    );
    let applied = revision_probe(events, "rate_applied", "request_revision", revision)
        .unwrap_or_else(|| panic!("{backend} lost the apply probe for {revision}"));
    let new_rate_start = revision_probe(events, "pcm_consumed", "render_revision", revision)
        .and_then(|event| event.field("output_start"))
        .unwrap_or_else(|| panic!("{backend} presented no PCM for {revision}"));
    let consumed_at_apply = at_apply
        .iter()
        .rfind(|event| event.probe == "pcm_consumed")
        .and_then(|event| event.field("output_end"))
        .unwrap_or_else(|| panic!("{backend} apply boundary has no presented transport"));
    let queued = usize::try_from(new_rate_start.saturating_sub(consumed_at_apply))
        .expect("queued output frames fit usize");
    let applied_rate = applied
        .field("applied_rate_bits")
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
        .map(|event| (event.field("source_frames"), event.field("output_frames")));
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
    response_source: PathBuf,
) {
    let (harness, queue) = playing_queue(temp_dir, backend, backends, case, response_source).await;
    let trace = usdt_trace::scope();
    let mut samples =
        capture_command_boundary(&harness, &trace, case.initial_tone, case.callback_frames).await;
    let command_frame = samples.len() / usize::from(CHANNELS);
    assert!(
        queue.is_playing(),
        "{backend} command boundary is not playing"
    );
    let ready = response_events(&trace);
    let published_end = ready
        .iter()
        .rfind(|event| event.probe == "publish")
        .and_then(|event| event.field("output_end"));
    let consumed_end = ready
        .iter()
        .rfind(|event| event.probe == "pcm_consumed")
        .and_then(|event| event.field("output_end"));
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
    let (acknowledged, revision, at_apply) = capture_until_applied(&harness, &trace, case).await;
    let apply_frame = command_frame + acknowledged.len() / usize::from(CHANNELS);
    samples.extend(acknowledged);
    samples.extend(capture_frames(&harness, case.observation_frames(), case.callback_frames).await);
    let events = response_events(&trace);
    drop(trace);

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
    all(
        not(target_os = "android"),
        not(all(target_os = "windows", target_env = "msvc"))
    ),
    case::bungee_minimum(StretchKind::Bungee, response_backends(), MINIMUM)
)]
#[cfg_attr(
    all(
        not(target_os = "android"),
        not(all(target_os = "windows", target_env = "msvc"))
    ),
    case::bungee_product(StretchKind::Bungee, response_backends(), PRODUCT)
)]
#[cfg_attr(
    all(
        not(target_os = "android"),
        not(all(target_os = "windows", target_env = "msvc"))
    ),
    case::bungee_extreme(StretchKind::Bungee, response_backends(), EXTREME)
)]
async fn a_live_rate_change_becomes_audible_within_the_pcm_already_rendered(
    temp_dir: TestTempDir,
    response_source: PathBuf,
    #[case] backend: StretchKind,
    #[case] backends: ElasticBackendConfig,
    #[case] case: ResponseCase,
) {
    run_case(&temp_dir, backend, backends, case, response_source).await;
}

fn assert_strict_response(
    backend: StretchKind,
    case: ResponseCase,
    command_frame: usize,
    samples: &[f32],
    events: &[ProbeEvent],
) {
    let requested = events
        .iter()
        .rfind(|event| event.probe == "rate_requested")
        .unwrap_or_else(|| {
            let publish = events
                .iter()
                .filter(|event| event.probe == "publish")
                .count();
            let consumed = events
                .iter()
                .filter(|event| event.probe == "pcm_consumed")
                .count();
            let applied = events
                .iter()
                .filter(|event| event.probe == "rate_applied")
                .count();
            panic!(
                "{backend} emitted no rate_requested probe; publish={publish}, rate_applied={applied}, pcm_consumed={consumed}"
            )
        });
    let revision = requested
        .field("request_revision")
        .unwrap_or_else(|| panic!("{backend} request probe has no revision"));
    assert_eq!(
        requested.field("target_rate_bits"),
        Some(u64::from(case.target_rate.to_bits())),
        "{backend} correlated the wrong final rate request"
    );
    let request_frame = requested
        .field("session_frame")
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
                .filter(|event| event.probe == "rate_applied")
                .map(|event| {
                    (
                        event.field("request_revision"),
                        event.field("session_frame"),
                        event
                            .field("applied_rate_bits")
                            .and_then(|bits| u32::try_from(bits).ok())
                            .map(f32::from_bits),
                        event.field("source_start"),
                        event.field("source_end"),
                    )
                })
                .collect();
            let presented: Vec<_> = events
                .iter()
                .filter(|event| event.probe == "pcm_consumed")
                .filter_map(|event| {
                    event
                        .field("render_revision")
                        .zip(event.field("output_start"))
                        .zip(event.field("output_end"))
                })
                .collect();
            let rendered: Vec<_> = events
                .iter()
                .filter(|event| event.probe == "render_committed")
                .filter_map(|event| {
                    event
                        .field("source_start")
                        .zip(event.field("source_end"))
                        .zip(event.field("output_start"))
                        .zip(event.field("output_end"))
                })
                .collect();
            let published: Vec<_> = events
                .iter()
                .filter(|event| event.probe == "publish")
                .filter_map(|event| {
                    event
                        .field("source")
                        .zip(event.field("output_start"))
                        .zip(event.field("output_end"))
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
        .field("session_frame")
        .and_then(|frame| i64::try_from(frame).ok())
        .unwrap_or_else(|| panic!("{backend} apply probe has no session frame"));
    let applied_rate = applied
        .field("applied_rate_bits")
        .and_then(|bits| u32::try_from(bits).ok())
        .map(f32::from_bits)
        .unwrap_or_else(|| panic!("{backend} apply probe has no rate"));
    let consumed_frame = consumed
        .field("output_start")
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
        .map(|event| (event.field("source_frames"), event.field("output_frames")));
    assert!(
        onset <= case.response_budget_frames,
        "{backend} target tone began after {onset} frames; applied after {applied_response}; presented after {presented_response}; budget is {}; primed={primed:?}",
        case.response_budget_frames
    );
}

async fn run_strict_case(
    temp_dir: &TestTempDir,
    backend: StretchKind,
    backends: ElasticBackendConfig,
    case: ResponseCase,
    response_source: PathBuf,
) {
    let (harness, queue) = playing_queue(temp_dir, backend, backends, case, response_source).await;
    let trace = usdt_trace::scope();
    let mut samples =
        capture_command_boundary(&harness, &trace, case.initial_tone, case.callback_frames).await;
    let command_frame = samples.len() / usize::from(CHANNELS);
    assert!(
        queue.is_playing(),
        "{backend} command boundary is not playing"
    );
    let ready = response_events(&trace);
    let published_end = ready
        .iter()
        .rfind(|event| event.probe == "publish")
        .and_then(|event| event.field("output_end"));
    let consumed_end = ready
        .iter()
        .rfind(|event| event.probe == "pcm_consumed")
        .and_then(|event| event.field("output_end"));
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
    let events = response_events(&trace);
    drop(trace);

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
    save_response_audio(backend, case, command_frame, &samples);
    assert_strict_response(backend, case, command_frame, &samples, &events);
    drop(queue);
    harness.close().await;
}

#[kithara::test(
    tokio,
    multi_thread,
    serial,
    timeout(Duration::from_secs(60)),
    hang_timeout_secs(5)
)]
#[case::signalsmith_default_quantum(
    StretchKind::Signalsmith,
    response_backends(),
    ResponseCase {
        render_quantum_frames: None,
        ..MINIMUM
    }
)]
#[case::signalsmith_minimum(StretchKind::Signalsmith, response_backends(), MINIMUM)]
#[case::signalsmith_product(StretchKind::Signalsmith, response_backends(), PRODUCT)]
#[case::signalsmith_extreme(StretchKind::Signalsmith, response_backends(), EXTREME)]
#[cfg_attr(
    all(
        not(target_os = "android"),
        not(all(target_os = "windows", target_env = "msvc"))
    ),
    case::bungee_minimum(StretchKind::Bungee, response_backends(), MINIMUM)
)]
#[cfg_attr(
    all(
        not(target_os = "android"),
        not(all(target_os = "windows", target_env = "msvc"))
    ),
    case::bungee_product(StretchKind::Bungee, response_backends(), PRODUCT)
)]
#[cfg_attr(
    all(
        not(target_os = "android"),
        not(all(target_os = "windows", target_env = "msvc"))
    ),
    case::bungee_extreme(StretchKind::Bungee, response_backends(), EXTREME)
)]
async fn live_rate_change_reaches_presented_pcm_within_response_budget(
    temp_dir: TestTempDir,
    response_source: PathBuf,
    #[case] backend: StretchKind,
    #[case] backends: ElasticBackendConfig,
    #[case] case: ResponseCase,
) {
    run_strict_case(&temp_dir, backend, backends, case, response_source).await;
}

/// A plain rate step is ramped by the existing Warp manual-rate smoother: no block moves the multiplier
/// further than the smoother's worst-case step for that block, the renderer
/// applies intermediate ratios, and the ramp settles at the target.
#[kithara::test(
    tokio,
    multi_thread,
    serial,
    timeout(Duration::from_secs(60)),
    hang_timeout_secs(5)
)]
#[case::signalsmith_ramp(StretchKind::Signalsmith, response_backends(), RAMP)]
async fn rate_multiplier_step_is_ramped_across_blocks(
    temp_dir: TestTempDir,
    response_source: PathBuf,
    #[case] backend: StretchKind,
    #[case] backends: ElasticBackendConfig,
    #[case] case: ResponseCase,
) {
    let (harness, queue) = playing_queue(&temp_dir, backend, backends, case, response_source).await;
    let trace = usdt_trace::scope();
    let mut samples =
        capture_command_boundary(&harness, &trace, case.initial_tone, case.callback_frames).await;
    let command_frame = samples.len() / usize::from(CHANNELS);
    let before_smoothed = trace.events_of("rate_smoothed").len();
    let before_applied = trace.events_of("rate_applied").len();
    queue.set_rate(case.target_rate);
    samples.extend(capture_frames(&harness, case.smooth_frames * 12, case.callback_frames).await);
    save_response_audio(backend, case, command_frame, &samples);
    let smoothing_events = trace.events_of("rate_smoothed");
    let smoothed: Vec<(u64, f32)> = smoothing_events[before_smoothed..]
        .iter()
        .filter_map(|event| {
            let bits =
                u32::try_from(event.field("multiplier_bits")?).expect("multiplier bits fit u32");
            Some((event.field("frames")?, f32::from_bits(bits)))
        })
        .collect();
    assert!(
        smoothed.len() >= 4,
        "{backend} emitted {} rate_smoothed blocks after the step",
        smoothed.len()
    );
    let delta = f64::from(case.target_rate - case.initial_rate);
    let smooth_frames =
        f64::from(u32::try_from(case.smooth_frames).expect("case smoothing fits u32"));
    for pair in smoothed.windows(2) {
        let (frames, next) = pair[1];
        let step = f64::from((next - pair[0].1).abs());
        let bound = delta * f64::from(u32::try_from(frames).expect("block fits u32"))
            / smooth_frames
            + 1e-3;
        assert!(
            step <= bound,
            "{backend} multiplier moved {step} over {frames} frames; {} smoothing frames allow {bound}: {smoothed:?}",
            case.smooth_frames
        );
    }
    assert!(
        smoothed
            .iter()
            .any(|(_, multiplier)| (multiplier - case.target_rate).abs() < 2e-3),
        "{backend} never settled at {}: {smoothed:?}",
        case.target_rate
    );
    let applied_events = trace.events_of("rate_applied");
    let applied: Vec<f32> = applied_events[before_applied..]
        .iter()
        .filter_map(|event| event.field("applied_rate_bits"))
        .map(|bits| f32::from_bits(u32::try_from(bits).expect("ratio bits fit u32")))
        .collect();
    let between = applied
        .iter()
        .filter(|ratio| {
            (*ratio - case.initial_rate).abs() > 1e-3 && (*ratio - case.target_rate).abs() > 1e-3
        })
        .count();
    assert!(
        between >= 3,
        "{backend} renderer never applied an intermediate ratio: {applied:?}"
    );
}

fn save_response_audio(
    backend: StretchKind,
    case: ResponseCase,
    command_frame: usize,
    samples: &[f32],
) {
    let name = format!(
        "rate-{backend}-{}-{}-{}-{}",
        case.callback_frames,
        case.source_block_frames,
        case.render_quantum_frames.map_or(0, NonZeroUsize::get),
        case.smooth_frames,
    );
    write_audio_artifact(
        &name,
        SAMPLE_RATE,
        CHANNELS,
        &[("output", samples)],
        &serde_json::json!({
            "backend": backend.to_string(),
            "command_frame": command_frame,
            "initial_rate": case.initial_rate,
            "target_rate": case.target_rate,
            "response_budget_frames": case.response_budget_frames,
            "smooth_frames": case.smooth_frames,
            "scope": "offline command to presented PCM; includes queued output",
        }),
    )
    .expect("listening artifact is saved before final response assertions");
}

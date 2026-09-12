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
    usdt_trace::{self, ProbeEvent},
    waits::wait_for_loader_done_event,
};
use kithara_test_fixtures::{assets::signal_mp3_sine880_30s, signal::goertzel_magnitude};

#[kithara::fixture]
fn response_source() -> &'static Path {
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
    target: usize,
    callback_frames: usize,
) -> Vec<f32> {
    let mut samples = Vec::new();
    for _ in 0..WARMUP_BLOCK_BUDGET {
        let block = capture_frames(harness, callback_frames, callback_frames).await;
        samples.extend_from_slice(&block);
        let frames = samples.len() / usize::from(CHANNELS);
        if frames >= TARGET_WINDOW_FRAMES
            && tone_is_dominant(
                &samples[(frames - TARGET_WINDOW_FRAMES) * usize::from(CHANNELS)..],
                target,
            )
        {
            return samples;
        }
    }
    panic!(
        "precondition: no callback presented the initial tone at a transport boundary; last_rms={}",
        signal_rms(&samples)
    );
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

fn assert_usdt_response(backend: StretchKind, case: ResponseCase, records: &[ProbeEvent]) {
    let target_rate_bits = u64::from(case.target_rate.to_bits());
    let request_index = records
        .iter()
        .rposition(|record| {
            record.probe == "rate_requested"
                && record.field("target_rate_bits") == Some(target_rate_bits)
        })
        .unwrap_or_else(|| {
            panic!("{backend} emitted no rate_requested USDT record for target rate")
        });
    let revision = records[request_index]
        .field("request_revision")
        .expect("rate_requested carries request_revision");
    let applied_index = records
        .iter()
        .enumerate()
        .skip(request_index + 1)
        .find_map(|(index, record)| {
            (record.probe == "rate_applied" && record.field("request_revision") == Some(revision))
                .then_some(index)
        })
        .unwrap_or_else(|| {
            panic!("{backend} emitted no rate_applied USDT record for revision {revision}")
        });
    let consumed_index = records
        .iter()
        .enumerate()
        .skip(applied_index + 1)
        .find_map(|(index, record)| {
            (record.probe == "pcm_consumed" && record.field("render_revision") == Some(revision))
                .then_some(index)
        })
        .unwrap_or_else(|| {
            panic!("{backend} emitted no pcm_consumed USDT record for revision {revision}")
        });
    let committed = records
        .iter()
        .skip(request_index + 1)
        .find(|record| record.probe == "render_committed")
        .unwrap_or_else(|| {
            panic!("{backend} emitted no render_committed USDT record after the request")
        });
    let requested = &records[request_index];
    let applied = &records[applied_index];
    let consumed = &records[consumed_index];
    assert_eq!(
        requested.field("target_rate_bits"),
        Some(target_rate_bits),
        "wrong requested target rate"
    );
    assert_eq!(
        applied.field("request_revision"),
        Some(revision),
        "wrong applied revision"
    );
    assert_eq!(
        consumed.field("render_revision"),
        Some(revision),
        "wrong consumed revision"
    );
    let applied_rate = f32::from_bits(
        u32::try_from(
            applied
                .field("applied_rate_bits")
                .expect("rate_applied carries rate"),
        )
        .expect("rate_applied rate bits fit into u32"),
    );
    if case.smooth_frames > 1 {
        let low = case.initial_rate.min(case.target_rate);
        let high = case.initial_rate.max(case.target_rate);
        assert!(
            applied_rate > low && applied_rate < high,
            "{backend} applied {applied_rate} instead of a smoothed rate between {low} and {high}"
        );
    }
    assert!(
        applied.field("source_start") <= applied.field("source_end"),
        "invalid applied source span"
    );
    assert!(
        consumed.field("output_start") <= consumed.field("output_end"),
        "invalid consumed output span"
    );
    assert!(
        consumed.field("source_start") <= consumed.field("source_end"),
        "invalid consumed source span"
    );
    assert!(
        committed.field("source_start") <= committed.field("source_end"),
        "invalid committed source span"
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
    let mut samples =
        capture_command_boundary(&harness, case.initial_tone, case.callback_frames).await;
    let command_frame = samples.len() / usize::from(CHANNELS);
    assert!(
        queue.is_playing(),
        "{backend} command boundary is not playing"
    );
    let trace = usdt_trace::scope();
    for command in 0..case.burst {
        queue.set_rate(if command.is_multiple_of(2) { 4.0 } else { 0.5 });
    }
    queue.set_rate(case.target_rate);
    samples.extend(capture_frames(&harness, case.observation_frames(), case.callback_frames).await);
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
    let records = trace.events();
    assert_usdt_response(backend, case, &records);
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

#![cfg(not(target_arch = "wasm32"))]

use std::{
    f64::consts::TAU,
    num::{NonZeroU32, NonZeroUsize},
};

#[cfg(feature = "perf")]
use hotpath::HotpathGuardBuilder;
use kithara::{
    platform::time::{self, Duration},
    play::{Resource, ResourceConfig, ResourceSrc},
    queue::{Queue, QueueConfig, Transition, test_utils::QueueProbe},
    warp::{StretchControls, StretchKind, WarpConfig},
};
use kithara_integration_tests::{
    TestTempDir,
    cochlea::{align_command_runs, first_sustained_delta},
    disk_asset_store, kithara,
    offline::{OfflinePlayerHarness, OfflinePlayerOptions},
    temp_dir,
};
use kithara_test_fixtures::{assets::signal_mp3_sine880_30s, signal::goertzel_magnitude};
use kithara_test_utils::probe::capture::{self as probe_capture, ProbeEvent};
use num_traits::AsPrimitive;

use crate::bufpool_ext::TestPools;

const SAMPLE_RATE: u32 = 44_100;
const CHANNELS: u16 = 2;
const BLOCK_FRAMES: usize = 128;
const INDUSTRY_RESPONSE_BUDGET_FRAMES: usize = 441;
const PRE_COMMAND_FRAMES: usize = BLOCK_FRAMES * 16;
const RESPONSE_OBSERVATION_FRAMES: usize = BLOCK_FRAMES * 32;
const SLOW_RATE: f32 = 0.5;
const INTERMEDIATE_RATE: f32 = 2.0;
const FAST_RATE: f32 = 4.0;
const TONES_HZ: [f64; 4] = [440.0, 880.0, 1_760.0, 3_520.0];
const TONE_DOMINANCE_RATIO: f64 = 4.0;
const MIN_SIGNAL_RMS: f64 = 0.003;
const PCM_DIFFERENCE_THRESHOLD: f32 = 0.002;
const PCM_DIFFERENCE_FRAMES: usize = 32;
const TARGET_WINDOW_FRAMES: usize = 128;
const RATE_COMMAND_BURST: usize = 64;
const STABLE_TONE_BLOCKS: usize = 4;
const WARMUP_BLOCK_BUDGET: usize = 200;

const UP: RateCase = RateCase::new(SLOW_RATE, 0, INTERMEDIATE_RATE, 2, false);
const DOWN: RateCase = RateCase::new(INTERMEDIATE_RATE, 2, SLOW_RATE, 0, false);
const EXTREME: RateCase = RateCase::new(SLOW_RATE, 0, FAST_RATE, 3, false);
const UNITY: RateCase = RateCase::new(1.0, 1, INTERMEDIATE_RATE, 2, false);
const BURST: RateCase = RateCase::new(SLOW_RATE, 0, INTERMEDIATE_RATE, 2, true);

#[derive(Clone, Copy, Debug)]
struct RateCase {
    initial_rate: f32,
    initial_tone: usize,
    target_rate: f32,
    target_tone: usize,
    burst: bool,
}

impl RateCase {
    const fn new(
        initial_rate: f32,
        initial_tone: usize,
        target_rate: f32,
        target_tone: usize,
        burst: bool,
    ) -> Self {
        Self {
            initial_rate,
            initial_tone,
            target_rate,
            target_tone,
            burst,
        }
    }
}

struct RateRun {
    command_frame: usize,
    probes: Vec<ProbeEvent>,
    response_budget_frames: usize,
    samples: Vec<f32>,
}

fn frame_period(frames: usize) -> Duration {
    Duration::from_secs_f64(
        f64::from(u32::try_from(frames).expect("render frame count fits u32"))
            / f64::from(SAMPLE_RATE),
    )
}

fn tone_magnitudes(samples: &[f32]) -> [f64; 4] {
    let frames = samples.len() / usize::from(CHANNELS);
    assert!(
        frames <= BLOCK_FRAMES,
        "tone probe exceeds its fixed window"
    );
    let mut channel = [0.0; BLOCK_FRAMES];
    for (sample, frame) in channel[..frames]
        .iter_mut()
        .zip(samples.chunks_exact(usize::from(CHANNELS)))
    {
        *sample = frame[0];
    }
    TONES_HZ.map(|tone| goertzel_magnitude(&channel[..frames], tone, SAMPLE_RATE))
}

fn tone_is_dominant(samples: &[f32], target: usize) -> bool {
    let magnitudes = tone_magnitudes(samples);
    signal_rms(samples) >= MIN_SIGNAL_RMS
        && magnitudes.iter().enumerate().all(|(index, magnitude)| {
            index == target || magnitudes[target] > magnitude * TONE_DOMINANCE_RATIO
        })
}

fn signal_rms(samples: &[f32]) -> f64 {
    let mut energy = 0.0;
    let mut frames = 0usize;
    for frame in samples.chunks_exact(usize::from(CHANNELS)) {
        energy = f64::from(frame[0]).mul_add(f64::from(frame[0]), energy);
        frames += 1;
    }
    if frames == 0 {
        return 0.0;
    }
    let frames = f64::from(u32::try_from(frames).expect("tone window fits u32"));
    (energy / frames).sqrt()
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

fn command_window(run: &RateRun) -> &[f32] {
    let channels = usize::from(CHANNELS);
    let start = run.command_frame - TARGET_WINDOW_FRAMES;
    &run.samples[start * channels..run.command_frame * channels]
}

async fn render_until_tone(harness: &OfflinePlayerHarness, target: usize, callback_frames: usize) {
    let mut stable_blocks = 0;
    for _ in 0..WARMUP_BLOCK_BUDGET {
        let block = harness.render(callback_frames);
        let _ = harness.tick_and_drain();
        let window_samples = TARGET_WINDOW_FRAMES * usize::from(CHANNELS);
        let stable = block.len() >= window_samples
            && tone_is_dominant(&block[block.len() - window_samples..], target);
        if stable {
            stable_blocks += 1;
        } else {
            stable_blocks = 0;
        }
        time::sleep(frame_period(callback_frames)).await;
        if stable_blocks == STABLE_TONE_BLOCKS {
            return;
        }
    }
    panic!("precondition: target tone {target} was not stable for {STABLE_TONE_BLOCKS} blocks");
}

async fn open_sine_resource(
    temp_dir: &TestTempDir,
    harness: &OfflinePlayerHarness,
) -> kithara::decode::DecodeResult<Resource> {
    let path = signal_mp3_sine880_30s()
        .path()
        .expect("generated sine fixture is stored on disk");
    let config: ResourceConfig<TestPools> = ResourceConfig::for_src(
        ResourceSrc::parse(path.to_str().expect("utf-8 fixture path"))
            .expect("local media path is a valid resource src"),
    )
    .store(disk_asset_store(temp_dir.path().join("live-rate-store")))
    .build();
    let config = harness
        .player()
        .prepare_config(config)
        .expect("offline player remains open");
    Resource::new(config).await
}

async fn playing_sine_queue(
    temp_dir: &TestTempDir,
    backend: StretchKind,
    initial_rate: f32,
    output_block_frames: usize,
    response_budget_frames: usize,
) -> (OfflinePlayerHarness, Queue<TestPools>, usize) {
    let stretch = StretchControls::new(1.0);
    stretch.set_backend(backend);
    let warp = WarpConfig::builder().stretch(stretch).build();
    let harness = OfflinePlayerHarness::with_sample_rate(
        OfflinePlayerOptions::builder()
            .crossfade_duration(0.0)
            .warp(warp)
            .output_block_frames(
                NonZeroU32::new(
                    u32::try_from(output_block_frames).expect("test output block fits u32"),
                )
                .expect("test output block is non-zero"),
            )
            .response_budget_frames(
                NonZeroUsize::new(response_budget_frames)
                    .expect("test response budget is non-zero"),
            )
            .build(),
        SAMPLE_RATE,
    );
    let resource = open_sine_resource(temp_dir, &harness)
        .await
        .expect("open local resource");
    let queue = Queue::new(
        QueueConfig::builder()
            .player(harness.take_player())
            .should_autoplay(false)
            .build(),
    );
    queue.set_default_rate(initial_rate);
    let id = queue.insert_loaded_for_test(resource);
    queue
        .select(id, Transition::None)
        .expect("select live-rate fixture");
    (harness, queue, response_budget_frames)
}

async fn capture_frames(
    harness: &OfflinePlayerHarness,
    frames: usize,
    pump_control: bool,
) -> Vec<f32> {
    let mut samples = Vec::with_capacity(frames * usize::from(CHANNELS));
    while samples.len() / usize::from(CHANNELS) < frames {
        let remaining = frames - samples.len() / usize::from(CHANNELS);
        let block_frames = remaining.min(BLOCK_FRAMES);
        samples.extend(harness.render(block_frames));
        if pump_control {
            let _ = harness.tick_and_drain();
        }
        time::sleep(frame_period(block_frames)).await;
    }
    samples
}

async fn response_run(
    temp_dir: &TestTempDir,
    backend: StretchKind,
    case: RateCase,
    change_rate: bool,
) -> RateRun {
    let (harness, queue, response_budget_frames) = playing_sine_queue(
        temp_dir,
        backend,
        case.initial_rate,
        BLOCK_FRAMES,
        INDUSTRY_RESPONSE_BUDGET_FRAMES,
    )
    .await;
    render_until_tone(&harness, case.initial_tone, BLOCK_FRAMES).await;

    let recorder = change_rate.then(probe_capture::install);
    let mut samples = capture_frames(&harness, PRE_COMMAND_FRAMES, true).await;
    let command_frame = samples.len() / usize::from(CHANNELS);
    if change_rate {
        if case.burst {
            for command in 0..RATE_COMMAND_BURST {
                queue.set_rate(if command.is_multiple_of(2) {
                    FAST_RATE
                } else {
                    SLOW_RATE
                });
            }
        }
        queue.set_rate(case.target_rate);
    } else {
        queue.set_rate(case.initial_rate);
    }
    samples.extend(capture_frames(&harness, RESPONSE_OBSERVATION_FRAMES, false).await);
    let probes = recorder.map_or_else(Vec::new, |recorder| recorder.snapshot());

    RateRun {
        command_frame,
        probes,
        response_budget_frames,
        samples,
    }
}

fn assert_response(
    backend: StretchKind,
    label: &str,
    case: RateCase,
    candidate: &RateRun,
    control: &RateRun,
) {
    assert_eq!(
        candidate.response_budget_frames, control.response_budget_frames,
        "{backend} {label} candidate and control use different response budgets"
    );
    let response_budget_frames = candidate.response_budget_frames;
    let aligned = align_command_runs(
        &candidate.samples,
        candidate.command_frame,
        &control.samples,
        control.command_frame,
        CHANNELS,
    );
    let aligned_frames = aligned.candidate.len() / usize::from(CHANNELS);
    assert!(
        first_sustained_delta(
            &aligned.candidate,
            &aligned.control,
            CHANNELS,
            0..aligned.command_frame,
            PCM_DIFFERENCE_THRESHOLD,
            PCM_DIFFERENCE_FRAMES,
        )
        .is_none(),
        "{backend} {label} candidate diverged from the no-command control before set_rate"
    );
    let candidate_command = command_window(candidate);
    let candidate_magnitudes = tone_magnitudes(candidate_command);
    let control_command = command_window(control);
    let control_magnitudes = tone_magnitudes(control_command);
    assert!(
        tone_is_dominant(candidate_command, case.initial_tone),
        "{backend} {label} candidate was not playing the initial tone before set_rate: \
         {candidate_magnitudes:?}"
    );
    assert!(
        tone_is_dominant(control_command, case.initial_tone),
        "{backend} {label} same-rate control was not playing the initial tone: \
         {control_magnitudes:?}"
    );
    assert!(
        !tone_is_dominant(candidate_command, case.target_tone),
        "{backend} {label} candidate already contained the target tone before set_rate"
    );
    assert!(
        first_target_onset(&control.samples, control.command_frame, case.initial_tone).is_some(),
        "{backend} {label} same-rate control stopped producing the initial tone"
    );
    assert!(
        first_target_onset(&control.samples, control.command_frame, case.target_tone).is_none(),
        "{backend} {label} same-rate control already contains the target tone"
    );

    let target_onset = first_target_onset(
        &candidate.samples,
        candidate.command_frame,
        case.target_tone,
    )
    .unwrap_or_else(|| {
        panic!(
            "{backend} {label} command never produced dominant {} Hz PCM",
            TONES_HZ[case.target_tone]
        )
    });
    let requested = candidate
        .probes
        .iter()
        .filter(|event| event.probe_name() == Some("rate_requested"))
        .max_by_key(|event| event.seq().unwrap_or(0))
        .unwrap_or_else(|| panic!("{backend} {label} emitted no rate_requested probe"));
    let revision = requested
        .u64("request_revision")
        .unwrap_or_else(|| panic!("{backend} {label} request probe has no revision"));
    let target_rate_bits = requested
        .u64("target_rate_bits")
        .unwrap_or_else(|| panic!("{backend} {label} request probe has no target rate"));
    let session_epoch = requested
        .u64("session_epoch")
        .unwrap_or_else(|| panic!("{backend} {label} request probe has no session epoch"));
    let request_frame = requested
        .u64("presentation_frame")
        .and_then(|frame| i64::try_from(frame).ok())
        .unwrap_or_else(|| panic!("{backend} {label} request probe has no presentation frame"));
    let preceding = candidate
        .probes
        .iter()
        .filter(|event| event.probe_name() == Some("pcm_consumed"))
        .filter(|event| event.u64("session_epoch") == Some(session_epoch))
        .filter(|event| {
            event
                .u64("output_end")
                .and_then(|frame| i64::try_from(frame).ok())
                == Some(request_frame)
        })
        .max_by_key(|event| event.u64("output_start"))
        .unwrap_or_else(|| {
            panic!("{backend} {label} captured no PCM ending at request boundary {request_frame}")
        });
    let preceding_start = preceding
        .u64("output_start")
        .and_then(|frame| i64::try_from(frame).ok())
        .unwrap_or_else(|| panic!("{backend} {label} preceding PCM has no start boundary"));
    let preceding_end = preceding
        .u64("output_end")
        .and_then(|frame| i64::try_from(frame).ok())
        .unwrap_or_else(|| panic!("{backend} {label} preceding PCM has no end boundary"));
    assert_eq!(
        preceding_end, request_frame,
        "{backend} {label} request transport does not match the presented PCM boundary"
    );
    assert!(
        preceding_start < preceding_end,
        "{backend} {label} command precondition has an empty PCM span"
    );
    assert_eq!(
        target_rate_bits,
        u64::from(case.target_rate.to_bits()),
        "{backend} {label} correlated the wrong final rate request"
    );

    let applied = candidate
        .probes
        .iter()
        .filter(|event| event.probe_name() == Some("rate_applied"))
        .filter(|event| event.u64("request_revision") == Some(revision))
        .filter(|event| event.u64("session_epoch") == Some(session_epoch))
        .filter(|event| event.u64("session_frame").is_some())
        .min_by_key(|event| event.u64("session_frame"))
        .unwrap_or_else(|| {
            panic!(
                "{backend} {label} emitted no successful rate_applied probe for revision \
                 {revision}"
            )
        });
    let applied_frame = applied
        .u64("session_frame")
        .and_then(|frame| i64::try_from(frame).ok())
        .unwrap_or_else(|| panic!("{backend} {label} rate_applied has no session frame"));
    let applied_rate = applied
        .u64("applied_rate_bits")
        .and_then(|bits| u32::try_from(bits).ok())
        .map(f32::from_bits)
        .unwrap_or_else(|| panic!("{backend} {label} rate_applied has no applied rate"));
    let moved_toward_target = if case.target_rate > case.initial_rate {
        case.initial_rate < applied_rate && applied_rate <= case.target_rate
    } else {
        case.target_rate <= applied_rate && applied_rate < case.initial_rate
    };
    assert!(
        moved_toward_target,
        "{backend} {label} first committed smooth rate {applied_rate} did not move from {} toward \
         {}",
        case.initial_rate, case.target_rate
    );
    let nominal_applied_response = applied_frame.checked_sub(request_frame).unwrap_or_else(|| {
        panic!(
            "{backend} {label} applied revision {revision} at frame {applied_frame} before its request \
             boundary {request_frame}"
        )
    });
    let applied_seq = applied
        .seq()
        .unwrap_or_else(|| panic!("{backend} {label} rate_applied has no causal sequence"));

    let pcm_response = first_sustained_delta(
        &aligned.candidate,
        &aligned.control,
        CHANNELS,
        aligned.command_frame..aligned_frames,
        PCM_DIFFERENCE_THRESHOLD,
        PCM_DIFFERENCE_FRAMES,
    )
    .and_then(|frame| frame.checked_sub(aligned.command_frame))
    .unwrap_or_else(|| panic!("{backend} {label} command never changed presented PCM"));

    let consumed = candidate
        .probes
        .iter()
        .filter(|event| event.probe_name() == Some("pcm_consumed"))
        .filter(|event| event.u64("session_epoch") == Some(session_epoch))
        .filter(|event| event.u64("render_revision") == Some(revision))
        .filter(|event| {
            event
                .u64("output_end")
                .and_then(|frame| i64::try_from(frame).ok())
                .is_some_and(|frame| frame > request_frame)
        })
        .filter(|event| event.u64("output_start").is_some())
        .min_by_key(|event| event.u64("output_start"))
        .unwrap_or_else(|| {
            panic!("{backend} {label} never presented PCM rendered for revision {revision}")
        });
    let consumed_output_start = consumed
        .u64("output_start")
        .and_then(|frame| i64::try_from(frame).ok())
        .unwrap_or_else(|| panic!("{backend} {label} pcm_consumed has no output start"));
    let consumed_output_end = consumed
        .u64("output_end")
        .and_then(|frame| i64::try_from(frame).ok())
        .unwrap_or_else(|| panic!("{backend} {label} pcm_consumed has no output boundary"));
    assert!(
        consumed_output_start < consumed_output_end,
        "{backend} {label} revision {revision} reached an empty presented span"
    );
    let consumed_seq = consumed
        .seq()
        .unwrap_or_else(|| panic!("{backend} {label} pcm_consumed has no causal sequence"));
    assert!(
        applied_seq < consumed_seq,
        "{backend} {label} revision {revision} was presented before its DSP quantum committed"
    );
    let consumed_frame = consumed_output_start;
    let consumed_response = consumed_frame.checked_sub(request_frame).unwrap_or_else(|| {
        panic!(
            "{backend} {label} consumed revision {revision} at frame {consumed_frame} before its \
             request boundary {request_frame}"
        )
    });
    let mut presented_frontier = request_frame;
    for event in candidate
        .probes
        .iter()
        .filter(|event| event.probe_name() == Some("pcm_consumed"))
        .filter(|event| event.u64("session_epoch") == Some(session_epoch))
    {
        let Some((start, end)) = event
            .u64("output_start")
            .and_then(|frame| i64::try_from(frame).ok())
            .zip(
                event
                    .u64("output_end")
                    .and_then(|frame| i64::try_from(frame).ok()),
            )
        else {
            continue;
        };
        if end <= request_frame || start >= consumed_frame {
            continue;
        }
        assert_eq!(
            start, presented_frontier,
            "{backend} {label} reached the new-rate source boundary after {consumed_response} \
             frames through a PCM gap or overlap"
        );
        presented_frontier = end.min(consumed_frame);
        if presented_frontier == consumed_frame {
            break;
        }
    }
    assert_eq!(
        presented_frontier, consumed_frame,
        "{backend} {label} reached the new-rate source boundary after {consumed_response} frames \
         through a partial PCM underrun"
    );
    assert!(
        consumed_response <= i64::try_from(response_budget_frames).unwrap_or(i64::MAX),
        "{backend} {label} revision {revision} reached presented PCM after {consumed_response} \
         frames; direct PCM divergence starts after {pcm_response} frames; hard budget is \
         {response_budget_frames} frames"
    );
    assert!(
        target_onset <= response_budget_frames,
        "{backend} {label} target {} Hz first sustained {TARGET_WINDOW_FRAMES}-frame window began \
         at {target_onset} frames \
         after revision \
         {revision}; smooth rate {applied_rate} first committed against its nominal transport at \
         {nominal_applied_response} frames and revision-stamped PCM reached presentation at \
         {consumed_response} frames; hard budget is {response_budget_frames} frames",
        TONES_HZ[case.target_tone],
    );
}

async fn run_response_case(
    temp_dir: &TestTempDir,
    backend: StretchKind,
    case: RateCase,
    label: &str,
) {
    let control = response_run(temp_dir, backend, case, false).await;

    #[cfg(feature = "perf")]
    let _guard = HotpathGuardBuilder::new("live_rate_response").build();

    let candidate = response_run(temp_dir, backend, case, true).await;
    assert_response(backend, label, case, &candidate, &control);
}

#[kithara::test(
    tokio,
    multi_thread,
    serial,
    flash(false),
    timeout(Duration::from_secs(30)),
    hang_timeout_secs(5)
)]
#[case::signalsmith_up(StretchKind::Signalsmith, UP)]
#[case::signalsmith_down(StretchKind::Signalsmith, DOWN)]
#[case::signalsmith_extreme(StretchKind::Signalsmith, EXTREME)]
#[case::signalsmith_unity(StretchKind::Signalsmith, UNITY)]
#[cfg_attr(
    not(all(target_os = "windows", target_env = "msvc")),
    case::bungee_up(StretchKind::Bungee, UP)
)]
#[cfg_attr(
    not(all(target_os = "windows", target_env = "msvc")),
    case::bungee_down(StretchKind::Bungee, DOWN)
)]
#[cfg_attr(
    not(all(target_os = "windows", target_env = "msvc")),
    case::bungee_extreme(StretchKind::Bungee, EXTREME)
)]
#[cfg_attr(
    not(all(target_os = "windows", target_env = "msvc")),
    case::bungee_unity(StretchKind::Bungee, UNITY)
)]
async fn live_rate_change_reaches_presented_pcm_within_response_budget(
    temp_dir: TestTempDir,
    #[case] backend: StretchKind,
    #[case] case: RateCase,
) {
    run_response_case(&temp_dir, backend, case, "live rate change").await;
}

#[kithara::test(
    tokio,
    multi_thread,
    serial,
    flash(false),
    timeout(Duration::from_secs(30)),
    hang_timeout_secs(5)
)]
#[case::signalsmith(StretchKind::Signalsmith)]
#[cfg_attr(
    not(all(target_os = "windows", target_env = "msvc")),
    case::bungee(StretchKind::Bungee)
)]
async fn latest_rate_wins_after_a_control_burst(
    temp_dir: TestTempDir,
    #[case] backend: StretchKind,
) {
    run_response_case(&temp_dir, backend, BURST, "latest-wins burst").await;
}

#[kithara::test(
    tokio,
    multi_thread,
    serial,
    flash(false),
    timeout(Duration::from_secs(30)),
    hang_timeout_secs(5)
)]
#[case::signalsmith_128(
    StretchKind::Signalsmith,
    BLOCK_FRAMES,
    INDUSTRY_RESPONSE_BUDGET_FRAMES
)]
#[case::signalsmith_512(StretchKind::Signalsmith, 512, 639)]
#[cfg_attr(
    not(all(target_os = "windows", target_env = "msvc")),
    case::bungee_128(StretchKind::Bungee, BLOCK_FRAMES, INDUSTRY_RESPONSE_BUDGET_FRAMES)
)]
#[cfg_attr(
    not(all(target_os = "windows", target_env = "msvc")),
    case::bungee_512(StretchKind::Bungee, 512, 639)
)]
async fn configured_output_buffer_keeps_consecutive_callbacks_contiguous(
    temp_dir: TestTempDir,
    #[case] backend: StretchKind,
    #[case] callback_frames: usize,
    #[case] response_budget_frames: usize,
) {
    let (harness, _queue, _) = playing_sine_queue(
        &temp_dir,
        backend,
        SLOW_RATE,
        callback_frames,
        response_budget_frames,
    )
    .await;
    render_until_tone(&harness, 0, callback_frames).await;

    let recorder = probe_capture::install();
    let first = harness.render(callback_frames);
    time::sleep(frame_period(callback_frames)).await;
    let second = harness.render(callback_frames);
    let _ = harness.tick_and_drain();
    let probes = recorder.snapshot();

    assert!(
        signal_rms(&first) >= MIN_SIGNAL_RMS && signal_rms(&second) >= MIN_SIGNAL_RMS,
        "{backend} {callback_frames}-frame callback contained an audible underrun"
    );

    let mut spans: Vec<(i64, i64, u64, u64, u64)> = probes
        .iter()
        .filter(|event| event.probe_name() == Some("pcm_consumed"))
        .filter_map(|event| {
            event
                .u64("output_start")
                .and_then(|frame| i64::try_from(frame).ok())
                .zip(
                    event
                        .u64("output_end")
                        .and_then(|frame| i64::try_from(frame).ok()),
                )
                .zip(event.u64("render_revision"))
                .zip(event.u64("source_start"))
                .zip(event.u64("source_end"))
                .map(|((((start, end), revision), source_start), source_end)| {
                    (start, end, revision, source_start, source_end)
                })
        })
        .collect();
    spans.sort_unstable();
    let observed = spans.clone();
    let Some(&(start, _, _, _, _)) = spans.first() else {
        panic!("{backend} {callback_frames}-frame callback emitted no presented PCM probes");
    };
    let mut frontier = start;
    for (span_start, span_end, _, _, _) in spans {
        assert_eq!(
            span_start, frontier,
            "{backend} {callback_frames}-frame callbacks contain a PCM gap or overlap: \
             {observed:?}"
        );
        assert!(span_start < span_end, "presented PCM span must advance");
        frontier = span_end;
    }
    assert_eq!(
        frontier - start,
        i64::try_from(callback_frames * 2).expect("test callback extent fits i64"),
        "{backend} output buffer did not cover two complete callbacks"
    );
}

#[kithara::test(
    tokio,
    multi_thread,
    serial,
    flash(false),
    timeout(Duration::from_secs(30)),
    hang_timeout_secs(5)
)]
#[case::industry_block(128, 441, true)]
#[case::oversized_block(512, 441, false)]
#[case::expanded_budget(512, 639, true)]
async fn session_output_block_owns_response_geometry(
    temp_dir: TestTempDir,
    #[case] output_block_frames: usize,
    #[case] response_budget_frames: usize,
    #[case] accepted: bool,
) {
    let harness = OfflinePlayerHarness::with_sample_rate(
        OfflinePlayerOptions::builder()
            .output_block_frames(
                NonZeroU32::new(
                    u32::try_from(output_block_frames).expect("test output block fits u32"),
                )
                .expect("test output block is non-zero"),
            )
            .response_budget_frames(
                NonZeroUsize::new(response_budget_frames)
                    .expect("test response budget is non-zero"),
            )
            .build(),
        SAMPLE_RATE,
    );

    let opened = open_sine_resource(&temp_dir, &harness).await;
    if accepted {
        opened.expect("session output geometry fits the configured response budget");
    } else {
        assert!(matches!(
            opened,
            Err(kithara::decode::DecodeError::InvalidData {
                detail: "playback output buffer exceeds the configured response budget"
            })
        ));
    }
}

#[kithara::test(native, flash(false))]
fn short_precommand_runs_are_aligned_by_their_signal() {
    const COMMAND_FRAME: usize = 128;
    const SHIFT: usize = 7;

    let source: Vec<f32> = (1_u32..=512)
        .map(|frame| {
            let frame: f32 = frame.as_();
            1.0 / frame
        })
        .collect();
    let candidate = &source[SHIFT..SHIFT + 384];
    let control = &source[..384];
    let aligned = align_command_runs(candidate, COMMAND_FRAME, control, COMMAND_FRAME, 1);

    assert_eq!(aligned.command_frame, COMMAND_FRAME);
    assert_eq!(aligned.candidate, aligned.control);
}

#[kithara::test(native, flash(false))]
fn target_window_distinguishes_every_rate_across_phase() {
    let channels = usize::from(CHANNELS);
    for amplitude in [1.0, 0.01] {
        for (target, tone) in TONES_HZ.into_iter().enumerate() {
            for phase_step in 0..128_u32 {
                let phase = TAU * f64::from(phase_step) / 128.0;
                let mut samples = Vec::with_capacity(TARGET_WINDOW_FRAMES * channels);
                for frame in 0..TARGET_WINDOW_FRAMES {
                    let frame = f64::from(u32::try_from(frame).expect("tone window fits u32"));
                    let sample: f32 = ((phase + TAU * tone * frame / f64::from(SAMPLE_RATE)).sin()
                        * amplitude)
                        .as_();
                    samples.extend(std::iter::repeat_n(sample, channels));
                }
                assert!(
                    tone_is_dominant(&samples, target),
                    "{tone} Hz target at amplitude {amplitude} is ambiguous at phase step \
                     {phase_step}"
                );
                for other in 0..TONES_HZ.len() {
                    if other != target {
                        assert!(
                            !tone_is_dominant(&samples, other),
                            "{tone} Hz was misclassified as {} Hz at amplitude {amplitude} and \
                             phase step {phase_step}",
                            TONES_HZ[other]
                        );
                    }
                }
            }
        }
    }
    let silence = vec![0.0; TARGET_WINDOW_FRAMES * channels];
    assert!(
        (0..TONES_HZ.len()).all(|tone| !tone_is_dominant(&silence, tone)),
        "silence must not satisfy any tone oracle"
    );
    let mut below_floor = Vec::with_capacity(TARGET_WINDOW_FRAMES * channels);
    for frame in 0..TARGET_WINDOW_FRAMES {
        let frame = f64::from(u32::try_from(frame).expect("tone window fits u32"));
        let sample: f32 =
            ((TAU * TONES_HZ[0] * frame / f64::from(SAMPLE_RATE)).sin() * 0.001).as_();
        below_floor.extend(std::iter::repeat_n(sample, channels));
    }
    assert!(
        (0..TONES_HZ.len()).all(|tone| !tone_is_dominant(&below_floor, tone)),
        "a tone below the signal floor must not satisfy any tone oracle"
    );
}

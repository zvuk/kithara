#![cfg(target_os = "macos")]

use cochlea_features::{Audio as ProbeAudio, SegmentOpts, segment_timeline};
use kithara::{
    StretchKind,
    audio::DecoderResamplerSettings,
    events::{ResamplerKind, TrackId},
    platform::sync::Arc,
    play::{PlaybackResamplerBackend, ResourceSrc},
    warp::{StretchControls, WarpConfig},
};
use kithara_integration_tests::{
    TestServerHelper,
    cochlea::percentile_f32,
    fixture_protocol::DelayRule,
    offline::{OfflinePlayerHarness, OfflinePlayerOptions},
};

use super::*;

const HOST_SAMPLE_RATE: u32 = 48_000;
const CAPTURE_FRAME: usize = HOST_SAMPLE_RATE as usize * 2;
const OBSERVATION_FRAMES: usize = HOST_SAMPLE_RATE as usize * 3;
const TARGET_SEGMENT: usize = 3;
const TARGET_DELAY_MS: u64 = 600;
const MAX_EXTRA_SILENT_FRAMES: usize = 2;

#[derive(Clone, Copy, Debug)]
struct ResamplerObservation {
    backend: ResamplerKind,
    output_rate: u32,
    channels: u16,
}

#[derive(Default)]
struct Lifecycle {
    decoders: Vec<DecoderObservation>,
    resamplers: Vec<ResamplerObservation>,
}

struct DesktopPrepared {
    _temp: TestTempDir,
    harness: OfflinePlayerHarness,
    abr: AbrHandle,
    events: EventReceiver,
    capture_frame: i64,
}

#[derive(Debug)]
struct DesktopRender {
    capture_frame: i64,
    samples: Vec<f32>,
}

fn desktop_fixture() -> HlsFixtureBuilder {
    fixture().delay_rules(vec![DelayRule {
        segment_eq: Some(TARGET_SEGMENT),
        segment_gte: None,
        variant: Some(FLAC),
        delay_ms: TARGET_DELAY_MS,
    }])
}

fn drain_lifecycle(events: &mut EventReceiver, frame_end: usize, lifecycle: &mut Lifecycle) {
    loop {
        let envelope = match events.try_recv() {
            Ok(envelope) => envelope,
            Err(TryRecvError::Empty) => break,
            Err(error) => {
                panic!("desktop event stream became unreliable at frame {frame_end}: {error}")
            }
        };
        match envelope.event {
            Event::Decoder(DecoderEvent::DecoderChanged {
                backend,
                cause,
                codec,
                sample_rate,
                channels,
                variant,
                ..
            }) => lifecycle.decoders.push(DecoderObservation {
                frame_end,
                backend,
                cause,
                codec,
                sample_rate,
                channels,
                variant,
            }),
            Event::Decoder(DecoderEvent::ResamplerConfigured {
                backend,
                output_rate,
                channels,
                ..
            }) => lifecycle.resamplers.push(ResamplerObservation {
                backend,
                output_rate,
                channels,
            }),
            _ => {}
        }
    }
}

fn assert_rubato_route(observations: &[ResamplerObservation], label: &str) {
    assert!(
        observations.iter().any(|observation| {
            observation.backend == ResamplerKind::Rubato
                && observation.output_rate == HOST_SAMPLE_RATE
                && observation.channels == CHANNELS
        }),
        "{label} must configure the Kithara App Rubato route from {SAMPLE_RATE} Hz to {HOST_SAMPLE_RATE} Hz: {observations:?}",
    );
}

fn assert_initial_apple_decoder(observations: &[DecoderObservation], label: &str) {
    assert_eq!(
        observations.len(),
        1,
        "{label} must observe exactly one initial decoder lifecycle event: {observations:?}",
    );
    let event = observations[0];
    assert_eq!(event.backend, DecoderBackendKind::Apple, "{label} backend");
    assert_eq!(event.cause, DecoderChangeCause::Initial, "{label} cause");
    assert_eq!(event.codec, Some(AudioCodecKind::AacLc), "{label} codec",);
    assert_eq!(
        event.sample_rate, HOST_SAMPLE_RATE,
        "{label} host-rate decoder output",
    );
    assert_eq!(event.channels, CHANNELS, "{label} channels");
    assert_eq!(
        event.variant,
        Some(u32::try_from(AAC_HIGH).unwrap_or(u32::MAX)),
        "{label} initial variant",
    );
}

fn host_frame(position: f64, label: &str) -> i64 {
    let exact = position * f64::from(HOST_SAMPLE_RATE);
    let rounded = exact.round();
    assert!(
        (exact - rounded).abs() <= 1.0e-4,
        "{label} position is not an integer host frame: position={position:.12}s, frame={exact:.9}",
    );
    rounded
        .to_i64()
        .expect("validated host frame fits signed frame coordinates")
}

/// Desktop-graph twin of the module's `render_paced`; the guard keeps the
/// modelled playback clock on the same virtual clock as the fixture delays and
/// the decode worker.
#[kithara::flash(true)]
async fn render_paced(harness: &OfflinePlayerHarness, frames: usize) -> Vec<f32> {
    let block = harness.render(frames);
    let _ = harness.tick_and_drain();
    sleep(Duration::from_secs_f64(
        frames.to_f64().expect("render frame count fits f64") / f64::from(HOST_SAMPLE_RATE),
    ))
    .await;
    block
}

async fn prepare_desktop_player(master_url: &url::Url, label: &str) -> DesktopPrepared {
    kithara_integration_tests::apple_warmup::warm_if_apple(DecoderBackend::Apple);

    let timestretch = StretchControls::new(1.0);
    timestretch.set_backend(StretchKind::Signalsmith);
    let harness = OfflinePlayerHarness::with_sample_rate(
        OfflinePlayerOptions::builder()
            .warp(
                WarpConfig::builder()
                    .stretch(Arc::clone(&timestretch))
                    .build(),
            )
            .build(),
        HOST_SAMPLE_RATE,
    );
    let temp = TestTempDir::new();
    let bus = EventBus::new(4_096);
    let mut events = bus.subscribe();
    let decoder_defaults = kithara::audio::AudioDecoderConfig::builder()
        .resampler(
            DecoderResamplerSettings::builder()
                .backend(PlaybackResamplerBackend::default())
                .build(),
        )
        .build();
    let decoder = kithara::audio::AudioDecoderConfig::builder()
        .backend(DecoderBackend::Apple)
        .gapless_mode(decoder_defaults.gapless_mode())
        .maybe_resampler(decoder_defaults.resampler().cloned())
        .build();
    let config = ResourceConfig::for_src(
        ResourceSrc::parse(master_url.as_str()).expect("fixture master URL must be valid"),
    )
    .store(kithara_integration_tests::disk_asset_store(temp.path()))
    .decoder(decoder)
    .initial_abr_mode(AbrMode::manual(AAC_HIGH))
    .events(bus)
    .build();
    let config = harness
        .player()
        .prepare_config(config)
        .unwrap_or_else(|error| panic!("prepare {label} Kithara App resource: {error}"));
    let resource = Resource::new(config)
        .await
        .unwrap_or_else(|error| panic!("open {label} Kithara App resource: {error:?}"));
    assert_eq!(
        resource.spec().sample_rate.get(),
        HOST_SAMPLE_RATE,
        "{label} resource must expose the host-rate PCM contract",
    );
    let abr = resource
        .abr_handle()
        .unwrap_or_else(|| panic!("{label} HLS resource must expose an ABR handle"));
    harness.with_player(|player| {
        player.reserve_slots(1);
        player.replace_item(0, resource, TrackId::allocate());
        player
            .select_item(0, true)
            .unwrap_or_else(|error| panic!("select {label} Kithara App resource: {error}"));
    });

    let deadline = Instant::now() + Duration::from_secs(20);
    let mut active_blocks = 0usize;
    let mut lifecycle = Lifecycle::default();
    drain_lifecycle(&mut events, 0, &mut lifecycle);
    loop {
        let position = harness.player().position_seconds().unwrap_or(0.0);
        let current = usize::try_from(host_frame(position, label))
            .unwrap_or_else(|_| panic!("{label} host frame must be non-negative"));
        assert!(
            current <= CAPTURE_FRAME,
            "{label} passed the fixed capture frame: current={current}, capture={CAPTURE_FRAME}",
        );
        if current == CAPTURE_FRAME && active_blocks >= 2 {
            break;
        }
        let frames = CAPTURE_FRAME.saturating_sub(current).min(BLOCK_FRAMES);
        let frames = frames.max(1);
        let block = render_paced(&harness, frames).await;
        drain_lifecycle(&mut events, current.saturating_add(frames), &mut lifecycle);
        if block.chunks_exact(usize::from(CHANNELS)).any(|frame| {
            frame
                .iter()
                .any(|sample| sample.abs() > ACTIVE_SAMPLE_THRESHOLD)
        }) {
            active_blocks = active_blocks.saturating_add(1);
        } else {
            active_blocks = 0;
        }
        assert!(
            Instant::now() <= deadline,
            "timed out preparing {label}: current_variant={:?}, position={position:.3}",
            abr.current_variant_index(),
        );
    }

    assert_eq!(
        abr.current_variant_index(),
        Some(AAC_HIGH),
        "{label} must start on AAC high",
    );
    assert_initial_apple_decoder(&lifecycle.decoders, label);
    assert_rubato_route(&lifecycle.resamplers, label);
    let capture_frame = host_frame(harness.player().position_seconds().unwrap_or(0.0), label);
    DesktopPrepared {
        _temp: temp,
        harness,
        abr,
        events,
        capture_frame,
    }
}

async fn render_desktop(master_url: &url::Url, switch: bool) -> DesktopRender {
    let label = if switch {
        "kithara-app-aac-to-flac"
    } else {
        "kithara-app-control"
    };
    let mut prepared = prepare_desktop_player(master_url, label).await;
    if switch {
        prepared
            .abr
            .set_mode(AbrMode::manual(FLAC))
            .unwrap_or_else(|error| panic!("request Kithara App AAC-to-FLAC switch: {error}"));
    }

    let mut lifecycle = Lifecycle::default();
    let mut samples = Vec::with_capacity(OBSERVATION_FRAMES * usize::from(CHANNELS));
    while samples.len() / usize::from(CHANNELS) < OBSERVATION_FRAMES {
        let rendered_frames = samples.len() / usize::from(CHANNELS);
        let frames = (OBSERVATION_FRAMES - rendered_frames).min(BLOCK_FRAMES);
        samples.extend_from_slice(&render_paced(&prepared.harness, frames).await);
        drain_lifecycle(
            &mut prepared.events,
            samples.len() / usize::from(CHANNELS),
            &mut lifecycle,
        );
    }

    if switch {
        assert_eq!(
            prepared.abr.current_variant_index(),
            Some(FLAC),
            "Kithara App switch must apply the FLAC target",
        );
        assert_eq!(
            lifecycle.decoders.len(),
            1,
            "Kithara App switch must recreate exactly one decoder: {:?}",
            lifecycle.decoders,
        );
        let target = lifecycle.decoders[0];
        assert_eq!(target.backend, DecoderBackendKind::Apple);
        assert_eq!(target.cause, DecoderChangeCause::VariantSwitch);
        assert_eq!(target.codec, Some(AudioCodecKind::Flac));
        assert_eq!(target.sample_rate, HOST_SAMPLE_RATE);
        assert_eq!(target.channels, CHANNELS);
        assert_eq!(
            target.variant,
            Some(u32::try_from(FLAC).unwrap_or(u32::MAX))
        );
        assert_rubato_route(&lifecycle.resamplers, label);
    } else {
        assert_eq!(
            prepared.abr.current_variant_index(),
            Some(AAC_HIGH),
            "Kithara App control must remain on AAC high",
        );
        assert!(
            lifecycle.decoders.is_empty(),
            "Kithara App control must not recreate its decoder: {:?}",
            lifecycle.decoders,
        );
    }

    DesktopRender {
        capture_frame: prepared.capture_frame,
        samples,
    }
}

fn longest_desktop_silent_run(samples: &[f32]) -> usize {
    let mut current = 0usize;
    let mut longest = 0usize;
    for frame in samples.chunks_exact(usize::from(CHANNELS)) {
        if frame
            .iter()
            .all(|sample| sample.abs() <= ACTIVE_SAMPLE_THRESHOLD)
        {
            current = current.saturating_add(1);
            longest = longest.max(current);
        } else {
            current = 0;
        }
    }
    longest
}

fn desktop_silent_buckets(samples: &[f32]) -> usize {
    let audio = ProbeAudio {
        samples: samples.to_vec(),
        channels: CHANNELS,
        sample_rate: HOST_SAMPLE_RATE,
    };
    segment_timeline(
        &audio,
        &SegmentOpts::default().with_window_ms(COCHLEA_WINDOW_MS),
    )
    .segments
    .iter()
    .filter(|segment| segment.silent)
    .count()
}

fn assert_no_desktop_click(switched: &[f32], control: &[f32]) {
    assert_eq!(
        switched.len(),
        control.len(),
        "Kithara App switch and control windows must be time-aligned",
    );
    let channels = usize::from(CHANNELS);
    let frames = switched.len() / channels;
    let omega = std::f32::consts::TAU * SINE_HZ.to_f32().expect("fixture frequency fits f32")
        / HOST_SAMPLE_RATE
            .to_f32()
            .expect("host sample rate fits f32");
    let recurrence = 2.0 * omega.cos();

    for channel in 0..channels {
        let sample = |pcm: &[f32], frame: usize| pcm[frame * channels + channel];
        let mut excess_steps = Vec::with_capacity(frames.saturating_sub(1));
        let mut excess_residuals = Vec::with_capacity(frames.saturating_sub(2));
        let mut peak_step = (0.0f32, 0usize);
        let mut peak_residual = (0.0f32, 0usize);
        for frame in 1..frames {
            let switched_step = (sample(switched, frame) - sample(switched, frame - 1)).abs();
            let control_step = (sample(control, frame) - sample(control, frame - 1)).abs();
            let excess_step = (switched_step - control_step).max(0.0);
            excess_steps.push(excess_step);
            if excess_step > peak_step.0 {
                peak_step = (excess_step, frame);
            }
            if frame >= 2 {
                let switched_residual = (sample(switched, frame)
                    - recurrence * sample(switched, frame - 1)
                    + sample(switched, frame - 2))
                .abs();
                let control_residual = (sample(control, frame)
                    - recurrence * sample(control, frame - 1)
                    + sample(control, frame - 2))
                .abs();
                let excess_residual = (switched_residual - control_residual).max(0.0);
                excess_residuals.push(excess_residual);
                if excess_residual > peak_residual.0 {
                    peak_residual = (excess_residual, frame);
                }
            }
        }
        let step_limit =
            percentile_f32(&mut excess_steps, 999, 1_000).mul_add(ORACLE_RATIO, ORACLE_SLACK);
        let residual_limit =
            percentile_f32(&mut excess_residuals, 999, 1_000).mul_add(ORACLE_RATIO, ORACLE_SLACK);
        assert!(
            peak_step.0 <= step_limit,
            "Kithara App AAC-to-FLAC switch inserted a click on channel {channel}: excess_step={:.6} at host frame {}, limit={step_limit:.6}",
            peak_step.0,
            peak_step.1,
        );
        assert!(
            peak_residual.0 <= residual_limit,
            "Kithara App AAC-to-FLAC switch inserted a phase discontinuity on channel {channel}: excess_residual={:.6} at host frame {}, limit={residual_limit:.6}",
            peak_residual.0,
            peak_residual.1,
        );
    }
}

#[kithara::test(
    tokio,
    multi_thread,
    native,
    serial,
    timeout(Duration::from_secs(180)),
    hang_timeout_secs(3)
)]
async fn kithara_app_manual_aac_to_flac_switch_is_gapless() {
    let server = TestServerHelper::new().await;
    let created = server
        .create_hls(desktop_fixture())
        .await
        .expect("create delayed Kithara App AAC-to-FLAC fixture");
    let master_url = created.master_url();

    let control = render_desktop(&master_url, false).await;
    let switched = render_desktop(&master_url, true).await;
    assert_eq!(
        switched.capture_frame, control.capture_frame,
        "Kithara App switch and control must start at the same host frame",
    );
    let control_silence = longest_desktop_silent_run(&control.samples);
    let switched_silence = longest_desktop_silent_run(&switched.samples);
    assert!(
        switched_silence <= control_silence.saturating_add(MAX_EXTRA_SILENT_FRAMES),
        "Kithara App AAC-to-FLAC switch inserted an audible gap: switched={switched_silence} host frames, control={control_silence}, target_delay_ms={TARGET_DELAY_MS}",
    );
    let control_buckets = desktop_silent_buckets(&control.samples);
    let switched_buckets = desktop_silent_buckets(&switched.samples);
    assert!(
        switched_buckets <= control_buckets,
        "Cochlea found switch-only silence in the Kithara App path: switched={switched_buckets}, control={control_buckets}, target_delay_ms={TARGET_DELAY_MS}",
    );
    assert_no_desktop_click(&switched.samples, &control.samples);
}

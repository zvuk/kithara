#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

#[path = "quality_switch_continuity/continuity.rs"]
mod continuity;
#[path = "quality_switch_continuity/desktop.rs"]
mod desktop;
#[path = "quality_switch_continuity/timeline.rs"]
mod timeline;
#[path = "quality_switch_continuity/underrun.rs"]
mod underrun;

use std::num::NonZeroU32;

use kithara::{
    abr::{AbrHandle, AbrMode},
    decode::DecoderBackend,
    events::{
        AudioCodecKind, DecoderBackend as DecoderBackendKind, DecoderChangeCause, DecoderEvent,
        Event, EventBus, EventReceiver,
    },
    host::HostConfig,
    platform::{
        time::{Duration, Instant, sleep},
        tokio::sync::broadcast::error::TryRecvError,
    },
    play::{PlayWorker, PlayWorkerConfig, Resource, ResourceConfig, ResourceSrc},
    stream::AudioCodec,
};
use kithara_integration_tests::{
    HlsFixtureBuilder, TestTempDir,
    fixture_protocol::{
        GaplessEncoding, PackagedAudioRequest, PackagedAudioSource, PackagedAudioVariantOverride,
        PackagedSignal,
    },
    offline::OfflinePlayer,
};
use num_traits::ToPrimitive;

use crate::bufpool_ext::{TestPools, pools};

const SAMPLE_RATE: u32 = 44_100;
const CHANNELS: u16 = 2;
const BLOCK_FRAMES: usize = 512;
const SINE_HZ: f64 = 441.0;
const SEGMENT_SECS: f64 = 0.5;
const SEGMENTS_PER_VARIANT: usize = 16;
const REQUEST_AT_SECS: f64 = 1.0;
const ACTIVE_SAMPLE_THRESHOLD: f32 = 1.0e-3;
const ORACLE_RATIO: f32 = 3.0;
const ORACLE_SLACK: f32 = 2.0e-3;
const COCHLEA_WINDOW_MS: f64 = 20.0;

const AAC_LOW: usize = 0;
const AAC_HIGH: usize = 1;
const FLAC: usize = 2;

fn longest_silent_run(samples: &[f32]) -> usize {
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

/// Every ordered pair of the fixture's variants. A switch is characterised by
/// what changes across it — bitrate only, codec only, or both at once — so
/// covering a subset leaves the sharpest combinations (the two-step
/// `aac-low` <-> `flac`, which changes codec *and* jumps two quality steps)
/// untested.
const TRANSITIONS: [Transition; 6] = [
    Transition::new("aac-low-to-aac-high", AAC_LOW, AAC_HIGH),
    Transition::new("aac-high-to-aac-low", AAC_HIGH, AAC_LOW),
    Transition::new("aac-to-flac", AAC_HIGH, FLAC),
    Transition::new("flac-to-aac", FLAC, AAC_HIGH),
    Transition::new("aac-low-to-flac", AAC_LOW, FLAC),
    Transition::new("flac-to-aac-low", FLAC, AAC_LOW),
];

#[derive(Clone, Copy, Debug)]
struct Transition {
    label: &'static str,
    from: usize,
    to: usize,
}

impl Transition {
    const fn new(label: &'static str, from: usize, to: usize) -> Self {
        Self { label, from, to }
    }
}

struct PreparedPlayer {
    _temp: TestTempDir,
    player: OfflinePlayer,
    abr: AbrHandle,
    events: EventReceiver,
    capture_frame: i64,
}

#[derive(Debug)]
struct SwitchRender {
    samples: Vec<f32>,
    capture_frame: i64,
    applied_frame: usize,
    target_decoder_frame: usize,
}

#[derive(Debug)]
struct ControlRender {
    samples: Vec<f32>,
    capture_frame: i64,
}

#[derive(Clone, Copy, Debug)]
struct DecoderObservation {
    frame_end: usize,
    backend: DecoderBackendKind,
    cause: DecoderChangeCause,
    codec: Option<AudioCodecKind>,
    sample_rate: u32,
    channels: u16,
    variant: Option<u32>,
}

fn fixture() -> HlsFixtureBuilder {
    fixture_with_signal(PackagedSignal::Sine { freq_hz: SINE_HZ })
}

fn fixture_with_signal(signal: PackagedSignal) -> HlsFixtureBuilder {
    HlsFixtureBuilder::new()
        .variant_count(3)
        .variant_bandwidths(vec![64_000, 256_000, 768_000])
        .segments_per_variant(SEGMENTS_PER_VARIANT)
        .segment_duration_secs(SEGMENT_SECS)
        .packaged_audio(PackagedAudioRequest {
            codec: AudioCodec::AacLc,
            gapless_encoding: GaplessEncoding::None,
            bit_rate: Some(64_000),
            encoder_delay: None,
            start_frame: None,
            timescale: Some(SAMPLE_RATE),
            trailing_delay: None,
            source: PackagedAudioSource::Signal(signal),
            variant_overrides: vec![
                PackagedAudioVariantOverride {
                    bit_rate: Some(256_000),
                    variant: AAC_HIGH,
                    ..Default::default()
                },
                PackagedAudioVariantOverride {
                    bit_rate: Some(768_000),
                    codec: Some(AudioCodec::Flac),
                    variant: FLAC,
                    ..Default::default()
                },
            ],
            channels: CHANNELS,
            sample_rate: SAMPLE_RATE,
        })
}

fn variant_codec(variant: usize) -> AudioCodecKind {
    if variant == FLAC {
        AudioCodecKind::Flac
    } else {
        AudioCodecKind::AacLc
    }
}

fn decoder_backend_kind(backend: DecoderBackend) -> DecoderBackendKind {
    match backend {
        DecoderBackend::Symphonia => DecoderBackendKind::Symphonia,
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        DecoderBackend::Apple => DecoderBackendKind::Apple,
        #[cfg(target_os = "android")]
        DecoderBackend::Android => DecoderBackendKind::Android,
        _ => panic!("unsupported decoder backend in native quality-switch test: {backend:?}"),
    }
}

fn source_frame(position: f64, label: &str) -> i64 {
    let exact = position * f64::from(SAMPLE_RATE);
    let rounded = exact.round();
    assert!(
        (exact - rounded).abs() <= 1.0e-6,
        "{label} capture position is not an integer source frame: position={position:.12}s, frame={exact:.9}",
    );
    rounded
        .to_i64()
        .expect("validated source frame fits signed frame coordinates")
}

/// Source frame every render captures at: the first whole render block at or
/// after [`REQUEST_AT_SECS`], so the capture sits on the block grid the warm-up
/// loop advances by.
fn capture_frame_target() -> i64 {
    let request = REQUEST_AT_SECS * f64::from(SAMPLE_RATE);
    let blocks = (request / BLOCK_FRAMES.to_f64().expect("render block fits f64")).ceil();
    let frames = blocks * BLOCK_FRAMES.to_f64().expect("render block fits f64");
    frames
        .to_i64()
        .expect("capture frame target fits the source frame index")
}

fn observation_frames() -> usize {
    usize::try_from(SAMPLE_RATE)
        .expect("fixture sample rate fits usize")
        .saturating_mul(3)
}

fn target_pcm_frames() -> usize {
    usize::try_from(SAMPLE_RATE).expect("fixture sample rate fits usize") / 4
}

fn drain_decoder_events(events: &mut EventReceiver, frame_end: usize) -> Vec<DecoderObservation> {
    let mut decoder_events = Vec::new();
    loop {
        let envelope = match events.try_recv() {
            Ok(envelope) => envelope,
            Err(TryRecvError::Empty) => break,
            Err(error) => {
                panic!("decoder event stream became unreliable at frame {frame_end}: {error}")
            }
        };
        if let Event::Decoder(DecoderEvent::DecoderChanged {
            backend,
            cause,
            codec,
            sample_rate,
            channels,
            variant,
            ..
        }) = envelope.event
        {
            decoder_events.push(DecoderObservation {
                frame_end,
                backend,
                cause,
                codec,
                sample_rate,
                channels,
                variant,
            });
        }
    }
    decoder_events
}

fn assert_initial_decoder(
    decoder_events: &[DecoderObservation],
    initial_variant: usize,
    backend: DecoderBackend,
    label: &str,
) {
    let expected_variant = u32::try_from(initial_variant).expect("fixture variant fits u32");
    assert_eq!(
        decoder_events.len(),
        1,
        "{label} must observe exactly one initial decoder lifecycle event: {decoder_events:?}",
    );
    let event = decoder_events[0];
    assert_eq!(
        event.backend,
        decoder_backend_kind(backend),
        "{label} backend"
    );
    assert_eq!(event.cause, DecoderChangeCause::Initial, "{label} cause");
    assert_eq!(
        event.codec,
        Some(variant_codec(initial_variant)),
        "{label} codec",
    );
    assert_eq!(event.sample_rate, SAMPLE_RATE, "{label} sample rate");
    assert_eq!(event.channels, CHANNELS, "{label} channels");
    assert_eq!(event.variant, Some(expected_variant), "{label} variant");
}

async fn prepare_player(
    master_url: &url::Url,
    initial_variant: usize,
    backend: DecoderBackend,
    label: &str,
) -> PreparedPlayer {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    kithara_integration_tests::apple_warmup::warm_if_apple(backend);

    let temp = TestTempDir::new();
    let bus = EventBus::new(1_024);
    let mut events = bus.subscribe();
    let config: ResourceConfig<TestPools> = ResourceConfig::for_src(
        ResourceSrc::parse(master_url.as_str()).expect("fixture master URL must be valid"),
    )
    .store(kithara_integration_tests::disk_asset_store(temp.path()))
    .worker(PlayWorker::new(PlayWorkerConfig::builder(pools()).build()))
    .decoder(
        kithara::audio::AudioDecoderConfig::builder()
            .backend(backend)
            .build(),
    )
    .initial_abr_mode(AbrMode::manual(initial_variant))
    .events(bus)
    .build();
    let resource = Resource::new(config)
        .await
        .unwrap_or_else(|error| panic!("open {label} resource: {error:?}"));
    let abr = resource
        .abr_handle()
        .unwrap_or_else(|| panic!("{label} HLS resource must expose an ABR handle"));
    let mut player = OfflinePlayer::new(
        HostConfig::offline(pools())
            .sample_rate(NonZeroU32::new(SAMPLE_RATE).expect("sample rate is non-zero"))
            .build(),
    );
    player.load_and_fadein(resource);

    // Render to a capture point fixed in *frames*, not to whichever frame the
    // warm-up happens to stop on. A cold start can hand back a short block, and
    // then the capture drifts off the block grid by however much the pipeline
    // owed — which turns "every render captures the same source frame" from a
    // property of the fixture into a property of the host's load. Every render
    // now asks for exactly the frames still missing, so it lands on
    // `capture_frame` whatever the pipeline did on the way there.
    let capture_frame = capture_frame_target();
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut active_blocks = 0usize;
    let mut decoder_events = drain_decoder_events(&mut events, 0);
    loop {
        let rendered = source_frame(player.position(), label);
        let remaining = capture_frame.saturating_sub(rendered);
        let request = usize::try_from(remaining)
            .unwrap_or(BLOCK_FRAMES)
            .min(BLOCK_FRAMES);
        let block = render_paced(&mut player, request.max(1)).await;
        decoder_events.extend(drain_decoder_events(&mut events, 0));
        if block
            .iter()
            .any(|sample| sample.abs() > ACTIVE_SAMPLE_THRESHOLD)
        {
            active_blocks = active_blocks.saturating_add(1);
        } else {
            active_blocks = 0;
        }
        if abr.current_variant_index() == Some(initial_variant)
            && source_frame(player.position(), label) >= capture_frame
            && active_blocks >= 2
        {
            break;
        }
        assert!(
            Instant::now() <= deadline,
            "timed out preparing {label}: initial={initial_variant}, current={:?}, position={:.3}",
            abr.current_variant_index(),
            player.position(),
        );
    }

    assert_initial_decoder(&decoder_events, initial_variant, backend, label);
    assert_eq!(
        source_frame(player.position(), label),
        capture_frame,
        "{label} warm-up must stop exactly on the capture frame",
    );
    PreparedPlayer {
        _temp: temp,
        player,
        abr,
        events,
        capture_frame,
    }
}

/// Consume one block and let the modelled playback clock advance by exactly
/// its duration.
///
/// The guard is what puts that advance on the same clock as everything else
/// this test observes. Fixture delays already burn virtual time
/// (`release_after_delay`), and the decode worker is a registered pacer, so the
/// clock only moves once the worker has parked. Without the guard this `sleep`
/// is a real `tokio` timer — the test macro rewrites time calls in the test
/// body, not in the helpers it calls — and the consumer then advances at host
/// speed against a producer advancing at virtual speed. With a one-chunk output
/// ring that mismatch is audible: a loaded host drains the ring and the feeder
/// hands out zeros, so the length of the silence is a property of the machine
/// rather than of the pipeline under test.
#[kithara::flash(true)]
async fn render_paced(player: &mut OfflinePlayer, frames: usize) -> Vec<f32> {
    let block = player.render(frames);
    sleep(Duration::from_secs_f64(
        frames.to_f64().expect("render frame count fits f64") / f64::from(SAMPLE_RATE),
    ))
    .await;
    block
}

async fn render_switch(
    master_url: &url::Url,
    transition: Transition,
    backend: DecoderBackend,
) -> SwitchRender {
    let mut prepared = prepare_player(master_url, transition.from, backend, transition.label).await;
    prepared
        .abr
        .set_mode(AbrMode::manual(transition.to))
        .unwrap_or_else(|error| panic!("request {}: {error}", transition.label));

    let observation_frames = observation_frames();
    let mut samples = Vec::with_capacity(observation_frames * usize::from(CHANNELS));
    let mut applied_frame = None;
    let mut decoder_events = Vec::new();
    while samples.len() / usize::from(CHANNELS) < observation_frames {
        let rendered_frames = samples.len() / usize::from(CHANNELS);
        let remaining_frames = observation_frames.saturating_sub(rendered_frames);
        let render_frames = remaining_frames.min(BLOCK_FRAMES);
        samples.extend_from_slice(&render_paced(&mut prepared.player, render_frames).await);
        let rendered_frames = samples.len() / usize::from(CHANNELS);
        decoder_events.extend(drain_decoder_events(&mut prepared.events, rendered_frames));
        if applied_frame.is_none() && prepared.abr.current_variant_index() == Some(transition.to) {
            applied_frame = Some(rendered_frames);
        }
    }
    assert_eq!(
        samples.len(),
        observation_frames * usize::from(CHANNELS),
        "{} fixed observation window length",
        transition.label,
    );

    let applied_frame = applied_frame.unwrap_or_else(|| {
        panic!(
            "{} did not apply inside the observation window: current={:?}, frames={}",
            transition.label,
            prepared.abr.current_variant_index(),
            samples.len() / usize::from(CHANNELS),
        )
    });
    let expected_variant = u32::try_from(transition.to).expect("fixture variant fits u32");
    assert_eq!(
        decoder_events.len(),
        1,
        "{} must emit exactly one decoder lifecycle event after its variant request: {decoder_events:?}",
        transition.label,
    );
    let target_event = decoder_events[0];
    assert_eq!(
        target_event.backend,
        decoder_backend_kind(backend),
        "{} target backend",
        transition.label,
    );
    assert_eq!(
        target_event.cause,
        DecoderChangeCause::VariantSwitch,
        "{} switch cause",
        transition.label,
    );
    assert_eq!(
        target_event.codec,
        Some(variant_codec(transition.to)),
        "{} target codec",
        transition.label,
    );
    assert_eq!(
        target_event.sample_rate, SAMPLE_RATE,
        "{} target sample rate",
        transition.label,
    );
    assert_eq!(
        target_event.channels, CHANNELS,
        "{} target channels",
        transition.label,
    );
    assert_eq!(
        target_event.variant,
        Some(expected_variant),
        "{} target variant",
        transition.label,
    );
    let target_decoder_frame = target_event.frame_end;
    let active_after_target = samples
        .chunks_exact(usize::from(CHANNELS))
        .skip(target_decoder_frame.saturating_add(BLOCK_FRAMES))
        .filter(|frame| {
            frame
                .iter()
                .any(|sample| sample.abs() > ACTIVE_SAMPLE_THRESHOLD)
        })
        .count();
    let target_pcm_frames = target_pcm_frames();
    assert!(
        active_after_target >= target_pcm_frames,
        "{} target decoder did not deliver enough post-event PCM: active={active_after_target}, required={target_pcm_frames}, target_event_frame={target_decoder_frame}",
        transition.label,
    );
    SwitchRender {
        samples,
        capture_frame: prepared.capture_frame,
        applied_frame,
        target_decoder_frame,
    }
}

async fn render_no_switch_control(
    master_url: &url::Url,
    initial_variant: usize,
    backend: DecoderBackend,
    label: &str,
) -> ControlRender {
    let mut prepared = prepare_player(master_url, initial_variant, backend, label).await;
    prepared
        .abr
        .set_mode(AbrMode::manual(initial_variant))
        .unwrap_or_else(|error| panic!("request inert {label} control: {error}"));
    let observation_frames = observation_frames();
    let mut samples = Vec::with_capacity(observation_frames * usize::from(CHANNELS));
    let mut decoder_events = Vec::new();
    while samples.len() / usize::from(CHANNELS) < observation_frames {
        let rendered_frames = samples.len() / usize::from(CHANNELS);
        let remaining_frames = observation_frames.saturating_sub(rendered_frames);
        let render_frames = remaining_frames.min(BLOCK_FRAMES);
        samples.extend_from_slice(&render_paced(&mut prepared.player, render_frames).await);
        decoder_events.extend(drain_decoder_events(
            &mut prepared.events,
            samples.len() / usize::from(CHANNELS),
        ));
    }
    assert_eq!(
        samples.len(),
        observation_frames * usize::from(CHANNELS),
        "{label} fixed observation window length",
    );
    assert_eq!(
        prepared.abr.current_variant_index(),
        Some(initial_variant),
        "{label} negative control must remain on its initial variant",
    );
    assert!(
        decoder_events.is_empty(),
        "{label} no-switch control must not recreate its decoder after Initial: {decoder_events:?}",
    );
    ControlRender {
        samples,
        capture_frame: prepared.capture_frame,
    }
}

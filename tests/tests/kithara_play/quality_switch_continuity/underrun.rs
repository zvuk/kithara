use cochlea_features::{Audio as ProbeAudio, SegmentOpts, segment_timeline};
use kithara::{
    audio::{AudioConfig, AudioSession},
    hls::{Hls, HlsConfig},
    play::{PlayWorker, PlayWorkerConfig},
};
use kithara_integration_tests::{
    TestServerHelper, fixture_protocol::DelayRule, offline::resource_from_reader,
};

use super::*;

const OUTPUT_RING_CHUNKS: usize = 1;
const TARGET_SEGMENT_DELAY_MS: u64 = 250;
const SLOW_TARGET_SEGMENT_DELAY_MS: u64 = 600;
const COLD_TARGET_SEGMENT_DELAY_MS: u64 = 300;
const OBSERVATION_FRAMES: usize = SAMPLE_RATE as usize * 3;
const CAPTURE_FRAME: usize = SAMPLE_RATE as usize * 2;
const TARGET_SWITCH_SEGMENT: usize = 3;
const MAX_EXTRA_SILENT_FRAMES: usize = 2;

const AAC_TO_FLAC: Transition = Transition::new("aac-to-flac", AAC_HIGH, FLAC);

#[derive(Debug)]
struct Render {
    capture_frame: i64,
    samples: Vec<f32>,
}

fn delayed_rebuild_fixture() -> HlsFixtureBuilder {
    fixture().delay_rules(vec![DelayRule {
        segment_eq: Some(TARGET_SWITCH_SEGMENT),
        segment_gte: None,
        variant: Some(FLAC),
        delay_ms: TARGET_SEGMENT_DELAY_MS,
    }])
}

fn slow_target_rebuild_fixture() -> HlsFixtureBuilder {
    fixture().delay_rules(vec![DelayRule {
        segment_eq: Some(TARGET_SWITCH_SEGMENT),
        segment_gte: None,
        variant: Some(FLAC),
        delay_ms: SLOW_TARGET_SEGMENT_DELAY_MS,
    }])
}

/// Every segment of the target variant is slow, so the switch cannot be served
/// from anything the source variant already pulled: the transition has to plan,
/// fetch and prime the incoming variant over a link that never gets faster.
/// This is the shape a real switch has — the target is cold — whereas a rule
/// pinned to one segment index only bites when the switch happens to land on
/// exactly that segment.
fn cold_target_fixture(target: usize) -> HlsFixtureBuilder {
    fixture().delay_rules(vec![DelayRule {
        segment_eq: None,
        segment_gte: Some(0),
        variant: Some(target),
        delay_ms: COLD_TARGET_SEGMENT_DELAY_MS,
    }])
}

async fn prepare_tiny_ring_player(
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
    let hls = HlsConfig::for_url(master_url.clone())
        .store(kithara_integration_tests::disk_asset_store(temp.path()))
        .initial_abr_mode(AbrMode::manual(initial_variant))
        .events(bus.clone())
        .build();
    let worker = PlayWorker::new(
        PlayWorkerConfig::for_pools(
            kithara::bufpool::BytePool::default(),
            kithara::bufpool::SamplePool::default(),
        )
        .build(),
    );
    let config = AudioConfig::<Hls>::for_stream(hls)
        .decoder(
            kithara::audio::AudioDecoderConfig::builder()
                .backend(backend)
                .build(),
        )
        .events(bus)
        .audio_buffer_chunks(OUTPUT_RING_CHUNKS)
        .build();
    let audio = worker
        .open(config)
        .await
        .unwrap_or_else(|error| panic!("open {label} audio: {error:?}"));
    let abr = audio
        .abr_handle()
        .unwrap_or_else(|| panic!("{label} HLS audio must expose an ABR handle"));
    let mut player = OfflinePlayer::new(SAMPLE_RATE);
    player.load_and_fadein(resource_from_reader(audio), label);

    let deadline = Instant::now() + Duration::from_secs(15);
    let mut active_blocks = 0usize;
    let mut decoder_events = drain_decoder_events(&mut events, 0);
    loop {
        let block = render_paced(&mut player, BLOCK_FRAMES).await;
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
            && player.position() >= REQUEST_AT_SECS
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
    render_to_capture_frame(&mut player, label).await;
    let capture_frame = source_frame(player.position(), label);
    PreparedPlayer {
        _temp: temp,
        player,
        abr,
        events,
        capture_frame,
    }
}

async fn render_to_capture_frame(player: &mut OfflinePlayer, label: &str) {
    loop {
        let current = usize::try_from(source_frame(player.position(), label))
            .unwrap_or_else(|_| panic!("{label} capture frame must be non-negative"));
        assert!(
            current <= CAPTURE_FRAME,
            "{label} passed fixed capture frame before the switch: current={current}, capture={CAPTURE_FRAME}",
        );
        let remaining = CAPTURE_FRAME.saturating_sub(current);
        if remaining == 0 {
            return;
        }
        render_paced(player, remaining.min(BLOCK_FRAMES)).await;
    }
}

async fn render_tiny_ring_control(
    master_url: &url::Url,
    transition: Transition,
    backend: DecoderBackend,
) -> Render {
    let label = format!("{}-tiny-ring-control", transition.label);
    let mut prepared = prepare_tiny_ring_player(master_url, transition.from, backend, &label).await;
    let mut samples = Vec::with_capacity(OBSERVATION_FRAMES * usize::from(CHANNELS));
    let mut decoder_events = Vec::new();
    while samples.len() / usize::from(CHANNELS) < OBSERVATION_FRAMES {
        let frames = (OBSERVATION_FRAMES - samples.len() / usize::from(CHANNELS)).min(BLOCK_FRAMES);
        samples.extend_from_slice(&render_paced(&mut prepared.player, frames).await);
        decoder_events.extend(drain_decoder_events(
            &mut prepared.events,
            samples.len() / usize::from(CHANNELS),
        ));
    }
    assert_eq!(
        prepared.abr.current_variant_index(),
        Some(transition.from),
        "{label} must stay on the source variant",
    );
    assert!(
        decoder_events.is_empty(),
        "{label} must not recreate its decoder: {decoder_events:?}",
    );
    Render {
        capture_frame: prepared.capture_frame,
        samples,
    }
}

async fn render_tiny_ring_switch(
    master_url: &url::Url,
    transition: Transition,
    backend: DecoderBackend,
) -> Render {
    let label = format!("{}-tiny-ring-switch", transition.label);
    let mut prepared = prepare_tiny_ring_player(master_url, transition.from, backend, &label).await;
    prepared
        .abr
        .set_mode(AbrMode::manual(transition.to))
        .unwrap_or_else(|error| panic!("request {label}: {error}"));

    let mut samples = Vec::with_capacity(OBSERVATION_FRAMES * usize::from(CHANNELS));
    let mut decoder_events = Vec::new();
    while samples.len() / usize::from(CHANNELS) < OBSERVATION_FRAMES {
        let frames = (OBSERVATION_FRAMES - samples.len() / usize::from(CHANNELS)).min(BLOCK_FRAMES);
        samples.extend_from_slice(&render_paced(&mut prepared.player, frames).await);
        decoder_events.extend(drain_decoder_events(
            &mut prepared.events,
            samples.len() / usize::from(CHANNELS),
        ));
    }
    assert_eq!(
        prepared.abr.current_variant_index(),
        Some(transition.to),
        "{label} must apply the target variant",
    );
    assert_eq!(
        decoder_events.len(),
        1,
        "{label} must recreate exactly one decoder: {decoder_events:?}",
    );
    let event = decoder_events[0];
    assert_eq!(
        event.cause,
        DecoderChangeCause::VariantSwitch,
        "{label} switch cause",
    );
    let expected_variant = u32::try_from(transition.to).expect("fixture variant fits u32");
    assert_eq!(
        event.variant,
        Some(expected_variant),
        "{label} target variant"
    );
    Render {
        capture_frame: prepared.capture_frame,
        samples,
    }
}

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

fn cochlea_silent_buckets(samples: &[f32]) -> usize {
    let audio = ProbeAudio {
        samples: samples.to_vec(),
        channels: CHANNELS,
        sample_rate: SAMPLE_RATE,
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

#[kithara::test(
    tokio,
    multi_thread,
    native,
    serial,
    timeout(Duration::from_secs(180)),
    hang_timeout_secs(3)
)]
#[case::symphonia(DecoderBackend::Symphonia)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::apple(DecoderBackend::Apple)
)]
async fn delayed_target_rebuild_keeps_the_player_output_continuous(
    #[case] backend: DecoderBackend,
) {
    let server = TestServerHelper::new().await;
    let created = server
        .create_hls(delayed_rebuild_fixture())
        .await
        .expect("create delayed target quality-switch fixture");
    let master_url = created.master_url();

    let control = render_tiny_ring_control(&master_url, AAC_TO_FLAC, backend).await;
    let switched = render_tiny_ring_switch(&master_url, AAC_TO_FLAC, backend).await;
    assert_eq!(
        switched.capture_frame, control.capture_frame,
        "switch and no-switch control must start at the same source frame",
    );
    let control_silence = longest_silent_run(&control.samples);
    let switched_silence = longest_silent_run(&switched.samples);
    assert!(
        switched_silence <= control_silence.saturating_add(MAX_EXTRA_SILENT_FRAMES),
        "delayed target rebuild inserted an audible silent gap: switched={switched_silence} frames, control={control_silence} frames, target_delay_ms={TARGET_SEGMENT_DELAY_MS}",
    );
    let control_buckets = cochlea_silent_buckets(&control.samples);
    let switched_buckets = cochlea_silent_buckets(&switched.samples);
    assert!(
        switched_buckets <= control_buckets,
        "Cochlea found switch-only silent buckets during delayed target rebuild: switched={switched_buckets}, control={control_buckets}, target_delay_ms={TARGET_SEGMENT_DELAY_MS}",
    );
}

#[kithara::test(
    tokio,
    multi_thread,
    native,
    serial,
    timeout(Duration::from_secs(180)),
    hang_timeout_secs(3),
    tracing("kithara_audio=debug,kithara_decode=debug,kithara_hls=debug,kithara_play=debug")
)]
#[case::symphonia(DecoderBackend::Symphonia)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::apple(DecoderBackend::Apple)
)]
async fn manual_aac_to_flac_switch_has_no_gap_when_target_preparation_exceeds_decoder_lead(
    #[case] backend: DecoderBackend,
) {
    let server = TestServerHelper::new().await;
    let created = server
        .create_hls(slow_target_rebuild_fixture())
        .await
        .expect("create slow AAC-to-FLAC quality-switch fixture");
    let master_url = created.master_url();

    let control = render_tiny_ring_control(&master_url, AAC_TO_FLAC, backend).await;
    let switched = render_tiny_ring_switch(&master_url, AAC_TO_FLAC, backend).await;
    assert_eq!(
        switched.capture_frame, control.capture_frame,
        "switch and no-switch control must start at the same source frame",
    );
    let control_silence = longest_silent_run(&control.samples);
    let switched_silence = longest_silent_run(&switched.samples);
    assert!(
        switched_silence <= control_silence.saturating_add(MAX_EXTRA_SILENT_FRAMES),
        "manual AAC-to-FLAC switch inserted an audible silent gap: switched={switched_silence} frames, control={control_silence} frames, target_delay_ms={SLOW_TARGET_SEGMENT_DELAY_MS}",
    );
    let control_buckets = cochlea_silent_buckets(&control.samples);
    let switched_buckets = cochlea_silent_buckets(&switched.samples);
    assert!(
        switched_buckets <= control_buckets,
        "Cochlea found switch-only silent buckets during manual AAC-to-FLAC switch: switched={switched_buckets}, control={control_buckets}, target_delay_ms={SLOW_TARGET_SEGMENT_DELAY_MS}",
    );
}

#[kithara::test(
    tokio,
    multi_thread,
    native,
    serial,
    timeout(Duration::from_secs(600)),
    hang_timeout_secs(3),
    tracing("kithara_audio=debug,kithara_decode=debug,kithara_hls=debug,kithara_play=debug")
)]
#[case::symphonia(DecoderBackend::Symphonia)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::apple(DecoderBackend::Apple)
)]
async fn cold_target_variant_switch_keeps_playback_uninterrupted(#[case] backend: DecoderBackend) {
    let mut failures = Vec::new();
    for transition in TRANSITIONS {
        let server = TestServerHelper::new().await;
        let created = server
            .create_hls(cold_target_fixture(transition.to))
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "create cold-target fixture for {}: {error:?}",
                    transition.label
                )
            });
        let master_url = created.master_url();

        let control = render_tiny_ring_control(&master_url, transition, backend).await;
        let switched = render_tiny_ring_switch(&master_url, transition, backend).await;
        assert_eq!(
            switched.capture_frame, control.capture_frame,
            "{} switch and no-switch control must start at the same source frame",
            transition.label,
        );

        let control_silence = longest_silent_run(&control.samples);
        let switched_silence = longest_silent_run(&switched.samples);
        if switched_silence > control_silence.saturating_add(MAX_EXTRA_SILENT_FRAMES) {
            failures.push(format!(
                "{} inserted an audible silent gap: switched={switched_silence} frames, control={control_silence} frames",
                transition.label,
            ));
        }
        let control_buckets = cochlea_silent_buckets(&control.samples);
        let switched_buckets = cochlea_silent_buckets(&switched.samples);
        if switched_buckets > control_buckets {
            failures.push(format!(
                "{} produced switch-only silent buckets: switched={switched_buckets}, control={control_buckets}",
                transition.label,
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "switching to a cold target variant interrupted playback (delay_ms={COLD_TARGET_SEGMENT_DELAY_MS}): {}",
        failures.join("; "),
    );
}

use std::ops::Range;

use cochlea_features::{Audio as ProbeAudio, SegmentOpts, segment_timeline};
use kithara::{
    assets::{AssetStore, StorageBackend},
    platform::{time::SystemTime, tokio::join},
    play::{PlayWorker, PlayWorkerConfig},
    queue::ItemEvent,
};
use kithara_integration_tests::{CreatedHls, TestServerHelper, fixture_protocol::DelayRule};
use kithara_render::bridge::DeckSnapshot;

use super::*;
use crate::bufpool_ext::pools;

const OUTPUT_RING_CHUNKS: usize = 1;
const TARGET_SEGMENT_DELAY_MS: u64 = 250;
const SLOW_TARGET_SEGMENT_DELAY_MS: u64 = 600;
const COLD_TARGET_SEGMENT_DELAY_MS: u64 = 300;
const OBSERVATION_FRAMES: usize = SAMPLE_RATE as usize * 2;
const CAPTURE_FRAME: usize = SAMPLE_RATE as usize;
/// The target segment the switch at [`CAPTURE_FRAME`] lands on.
const TARGET_SWITCH_SEGMENT: usize = 1;
const MAX_EXTRA_SILENT_FRAMES: usize = 2;

const AAC_TO_FLAC: Transition = Transition::new("aac-to-flac", AAC_HIGH, FLAC);

#[derive(Debug)]
struct Render {
    capture_frame: i64,
    samples: Vec<f32>,
    blocks: Vec<CaptureBlock>,
}

#[derive(Debug)]
struct CaptureBlock {
    capture: Range<usize>,
    output: Range<u64>,
    started_unix_micros: Option<u128>,
    before: DeckSnapshot,
    after: DeckSnapshot,
    item_events: Vec<ItemEvent>,
    event_error: Option<String>,
    refill_probes: Vec<kithara_test_utils::test::usdt::ProbeEvent>,
}

impl CaptureBlock {
    fn diagnostic(&self) -> String {
        let lane_frames = self
            .before
            .slots
            .iter()
            .zip(&self.after.slots)
            .map(|(before, after)| {
                let before = before.mark?;
                let after = after.mark?;
                if before.lane.segment != after.lane.segment {
                    return None;
                }
                after.lane.frame.checked_sub(before.lane.frame)
            })
            .collect::<Vec<_>>();
        format!(
            "capture={:?} output={:?} started_unix_micros={:?} lane_frames={lane_frames:?} metrics_before={:?} metrics_after={:?} slots_before={:#?} slots_after={:#?} item_events={:?} event_error={:?} refill_probes={:?}",
            self.capture,
            self.output,
            self.started_unix_micros,
            self.before.metrics,
            self.after.metrics,
            self.before.slots,
            self.after.slots,
            self.item_events,
            self.event_error,
            self.refill_probes,
        )
    }
}

impl Render {
    fn silence_diagnostic(&self) -> String {
        let mut current = 0..0;
        let mut longest = 0..0;
        for (frame, samples) in self.samples.chunks_exact(usize::from(CHANNELS)).enumerate() {
            if samples
                .iter()
                .all(|sample| sample.abs() <= ACTIVE_SAMPLE_THRESHOLD)
            {
                if current.is_empty() {
                    current.start = frame;
                }
                current.end = frame + 1;
                if current.len() > longest.len() {
                    longest = current.clone();
                }
            } else {
                current = 0..0;
            }
        }
        let at = self.blocks.iter().position(|block| {
            block.capture.start <= longest.start && longest.start < block.capture.end
        });
        let around = at.map_or(0..0, |index| {
            index.saturating_sub(2)..(index + 3).min(self.blocks.len())
        });
        let nearby = self.blocks[around]
            .iter()
            .map(CaptureBlock::diagnostic)
            .collect::<Vec<_>>();
        let underruns = self
            .blocks
            .iter()
            .filter(|block| {
                block.after.metrics.underruns() > block.before.metrics.underruns()
                    || !block.item_events.is_empty()
                    || block.event_error.is_some()
            })
            .take(8)
            .map(CaptureBlock::diagnostic)
            .collect::<Vec<_>>();
        let output_silence = self.blocks.first().and_then(|block| {
            let start = block
                .output
                .start
                .checked_add(u64::try_from(longest.start).ok()?)?;
            let end = block
                .output
                .start
                .checked_add(u64::try_from(longest.end).ok()?)?;
            Some(start..end)
        });
        format!(
            "capture_source_frame={} longest_silence={longest:?} output_silence={output_silence:?} nearby=[{}] underrun_blocks_first_8=[{}]",
            self.capture_frame,
            nearby.join("\n"),
            underruns.join("\n"),
        )
    }
}

fn target_rebuild_fixture(delay_ms: u64) -> HlsFixtureBuilder {
    fixture().delay_rules(vec![DelayRule {
        segment_eq: Some(TARGET_SWITCH_SEGMENT),
        segment_gte: None,
        variant: Some(FLAC),
        delay_ms,
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

#[kithara::flash(io)]
#[kithara_test_utils::kithara::hang_watchdog]
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
    let pools = pools();
    let store = AssetStore::builder(pools.clone())
        .backend(StorageBackend::Disk {
            root: temp.path().to_path_buf(),
        })
        .build();
    let worker = PlayWorker::new(PlayWorkerConfig::builder(pools.clone()).build());
    let config =
        kithara::play::ResourceConfig::for_src(kithara::play::ResourceSrc::Url(master_url.clone()))
            .store(store)
            .worker(worker)
            .initial_abr_mode(AbrMode::manual(initial_variant))
            .decoder(
                kithara::audio::AudioDecoderConfig::builder()
                    .backend(backend)
                    .build(),
            )
            .events(bus)
            .preload_chunks(std::num::NonZeroUsize::MIN)
            .audio_buffer_chunks(
                std::num::NonZeroUsize::new(OUTPUT_RING_CHUNKS).expect("output ring depth"),
            )
            .build();
    let mut player = OfflinePlayer::new(
        HostConfig::offline(pools)
            .settings(
                HostSettings::builder()
                    .sample_rate(NonZeroU32::new(SAMPLE_RATE).expect("sample rate is non-zero"))
                    .build(),
            )
            .build(),
    )
    .await;
    let id = player.load_config(config).await;
    player
        .render_until_current(
            id,
            BLOCK_FRAMES,
            WallInstant::now() + kithara_integration_tests::offline::LOCAL_LOAD_DEADLINE,
        )
        .await;
    let abr = player
        .player()
        .current_abr_handle()
        .unwrap_or_else(|| panic!("{label} HLS audio must expose an ABR handle"));

    let mut pacer = Pacer::default();
    let deadline = WallInstant::now() + Duration::from_secs(15);
    let mut active_blocks = 0usize;
    let mut decoder_events = drain_decoder_events(&mut events, 0);
    loop {
        hang_tick!();
        let before = player.position();
        let block = render_paced(&mut player, &mut pacer, BLOCK_FRAMES).await;
        if player.position() > before {
            hang_reset!();
        }
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
            WallInstant::now() <= deadline,
            "timed out preparing {label}: initial={initial_variant}, current={:?}, position={:.3}",
            abr.current_variant_index(),
            player.position(),
        );
    }

    assert_initial_decoder(&decoder_events, initial_variant, backend, label);
    render_to_capture_frame(&mut player, &mut pacer, label).await;
    let capture_frame = source_frame(player.position(), label);
    PreparedPlayer {
        _temp: temp,
        player,
        abr,
        events,
        pacer,
        capture_frame,
    }
}

#[kithara::flash(io)]
#[kithara_test_utils::kithara::hang_watchdog]
async fn render_to_capture_frame(player: &mut OfflinePlayer, pacer: &mut Pacer, label: &str) {
    loop {
        hang_tick!();
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
        let before = player.position();
        render_paced(player, pacer, remaining.min(BLOCK_FRAMES)).await;
        if player.position() > before {
            hang_reset!();
        }
    }
}

#[kithara::flash(io)]
#[kithara_test_utils::kithara::hang_watchdog]
async fn capture_tiny_ring(
    prepared: &mut PreparedPlayer,
) -> (Vec<f32>, Vec<DecoderObservation>, Vec<CaptureBlock>) {
    let mut item_events = prepared.player.player().subscribe::<ItemEvent>();
    let mut blocks = Vec::with_capacity(OBSERVATION_FRAMES.div_ceil(BLOCK_FRAMES));
    let mut samples = Vec::with_capacity(OBSERVATION_FRAMES * usize::from(CHANNELS));
    let mut decoder_events = Vec::new();
    let mut refill_snapshots = 0usize;
    while samples.len() / usize::from(CHANNELS) < OBSERVATION_FRAMES {
        hang_tick!();
        let before = prepared.player.position();
        let frames = (OBSERVATION_FRAMES - samples.len() / usize::from(CHANNELS)).min(BLOCK_FRAMES);
        let capture_start = samples.len() / usize::from(CHANNELS);
        let output_start = prepared.player.host().position();
        let started_unix_micros = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .ok()
            .map(|duration| duration.as_micros());
        let snapshot_before = prepared.player.deck_snapshot();
        let block = render_paced(&mut prepared.player, &mut prepared.pacer, frames).await;
        let snapshot_after = prepared.player.deck_snapshot();
        let output_end = prepared.player.host().position();
        let short =
            snapshot_before
                .slots
                .iter()
                .zip(&snapshot_after.slots)
                .any(|(before, after)| {
                    before.mark.zip(after.mark).is_some_and(|(before, after)| {
                        before.lane.segment == after.lane.segment
                            && after.lane.frame.checked_sub(before.lane.frame).is_some_and(
                                |served| served < u64::try_from(frames).unwrap_or(u64::MAX),
                            )
                    })
                });
        let refill_probes = if short && refill_snapshots < 3 {
            refill_snapshots += 1;
            kithara_test_utils::test::usdt::events_of("feeder_refill_unavailable")
                .into_iter()
                .rev()
                .filter(|event| {
                    snapshot_after
                        .slots
                        .iter()
                        .filter_map(|slot| slot.mark)
                        .any(|mark| {
                            event.field("segment") == Some(mark.lane.segment.get())
                                && event.field("lane_frame") == Some(mark.lane.frame)
                        })
                })
                .take(8)
                .collect()
        } else {
            Vec::new()
        };
        let mut observed_events = Vec::new();
        let mut event_error = None;
        for _ in 0..32 {
            match item_events.try_recv() {
                Ok(envelope) => observed_events.push(envelope.event),
                Err(kithara::platform::tokio::sync::broadcast::error::TryRecvError::Empty) => break,
                Err(error) => {
                    event_error = Some(error.to_string());
                    break;
                }
            }
        }
        if observed_events.len() == 32 {
            event_error = Some("per-block ItemEvent capture reached 32 events".into());
        }
        blocks.push(CaptureBlock {
            capture: capture_start..capture_start + frames,
            output: output_start..output_end,
            started_unix_micros,
            before: snapshot_before,
            after: snapshot_after,
            item_events: observed_events,
            event_error,
            refill_probes,
        });
        samples.extend_from_slice(&block);
        if prepared.player.position() > before {
            hang_reset!();
        }
        decoder_events.extend(drain_decoder_events(
            &mut prepared.events,
            samples.len() / usize::from(CHANNELS),
        ));
    }
    (samples, decoder_events, blocks)
}

async fn render_tiny_ring_control(
    master_url: &url::Url,
    transition: Transition,
    backend: DecoderBackend,
) -> Render {
    let label = format!("{}-tiny-ring-control", transition.label);
    let mut prepared = prepare_tiny_ring_player(master_url, transition.from, backend, &label).await;
    let (samples, decoder_events, blocks) = capture_tiny_ring(&mut prepared).await;
    assert_eq!(
        prepared.abr.current_variant_index(),
        Some(transition.from),
        "{label} must stay on the source variant",
    );
    assert!(
        decoder_events.is_empty(),
        "{label} must not recreate its decoder: {decoder_events:?}",
    );
    let result = Render {
        capture_frame: prepared.capture_frame,
        samples,
        blocks,
    };
    prepared.close().await;
    result
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

    let (samples, decoder_events, blocks) = capture_tiny_ring(&mut prepared).await;
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
    assert!(
        event.frame_end.saturating_add(heard_after_switch_frames()) <= OBSERVATION_FRAMES,
        "{label} switched too late to be heard: target_event_frame={}, window={OBSERVATION_FRAMES}",
        event.frame_end,
    );
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
    let result = Render {
        capture_frame: prepared.capture_frame,
        samples,
        blocks,
    };
    prepared.close().await;
    result
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
    timeout(Duration::from_secs(8)),
    hang_timeout_secs(3),
    tracing(
        "kithara_audio=debug,kithara_decode=debug,kithara_hls=debug,kithara_play=debug,render_refill=trace"
    )
)]
#[cfg_attr(not(target_os = "android"), case::delayed_symphonia(
    DecoderBackend::Symphonia,
    TARGET_SEGMENT_DELAY_MS,
    "delayed target rebuild"
, target_source().await))]
#[cfg_attr(target_os = "android", case::delayed_android(
    DecoderBackend::default(),
    TARGET_SEGMENT_DELAY_MS,
    "delayed target rebuild"
, target_source().await))]
#[cfg_attr(not(target_os = "android"), case::slow_symphonia(
    DecoderBackend::Symphonia,
    SLOW_TARGET_SEGMENT_DELAY_MS,
    "manual AAC-to-FLAC switch"
, slow_source().await))]
#[cfg_attr(target_os = "android", case::slow_android(
    DecoderBackend::default(),
    SLOW_TARGET_SEGMENT_DELAY_MS,
    "manual AAC-to-FLAC switch"
, slow_source().await))]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::delayed_apple(
        DecoderBackend::Apple,
        TARGET_SEGMENT_DELAY_MS,
        "delayed target rebuild"
    , target_source().await)
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::slow_apple(
        DecoderBackend::Apple,
        SLOW_TARGET_SEGMENT_DELAY_MS,
        "manual AAC-to-FLAC switch"
    , slow_source().await)
)]
async fn target_rebuild_keeps_player_output_continuous(
    #[case] backend: DecoderBackend,
    #[case] delay_ms: u64,
    #[case] label: &str,
    #[case] source: (TestServerHelper, CreatedHls),
) {
    let _refill_scope = kithara_test_utils::test::usdt::scope();
    let (_server, created) = source;
    let master_url = created.master_url();

    // Played side by side, the control hears the same host load the switch does.
    let (control, switched) = join!(
        render_tiny_ring_control(&master_url, AAC_TO_FLAC, backend),
        render_tiny_ring_switch(&master_url, AAC_TO_FLAC, backend),
    );
    assert_eq!(
        switched.capture_frame, control.capture_frame,
        "switch and no-switch control must start at the same source frame",
    );
    let control_silence = longest_silent_run(&control.samples);
    let switched_silence = longest_silent_run(&switched.samples);
    assert!(
        switched_silence <= control_silence.saturating_add(MAX_EXTRA_SILENT_FRAMES),
        "{label} inserted an audible silent gap: switched={switched_silence} frames, control={control_silence} frames, target_delay_ms={delay_ms}; switched: {}; control: {}",
        switched.silence_diagnostic(),
        control.silence_diagnostic(),
    );
    let control_buckets = cochlea_silent_buckets(&control.samples);
    let switched_buckets = cochlea_silent_buckets(&switched.samples);
    assert!(
        switched_buckets <= control_buckets,
        "Cochlea found switch-only silent buckets during {label}: switched={switched_buckets}, control={control_buckets}, target_delay_ms={delay_ms}",
    );
}

#[kithara::test(
    tokio,
    multi_thread,
    native,
    serial,
    timeout(Duration::from_secs(30)),
    hang_timeout_secs(3),
    tracing(
        "kithara_audio=debug,kithara_decode=debug,kithara_hls=debug,kithara_play=debug,kithara_audio::pipeline::decode::transition=trace,kithara_audio::pipeline::source::variant=trace,render_refill=trace"
    )
)]
#[cfg_attr(not(target_os = "android"), case::symphonia(DecoderBackend::Symphonia))]
#[cfg_attr(target_os = "android", case::android(DecoderBackend::default()))]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::apple(DecoderBackend::Apple)
)]
async fn cold_target_variant_switch_keeps_playback_uninterrupted(
    #[case] backend: DecoderBackend,
    #[future(awt)] cold_sources: Vec<(TestServerHelper, CreatedHls)>,
) {
    let _refill_scope = kithara_test_utils::test::usdt::scope();
    let sources: Vec<(Transition, url::Url)> = TRANSITIONS
        .into_iter()
        .zip(&cold_sources)
        .map(|(transition, (_server, created))| (transition, created.master_url()))
        .collect();
    let mut failures = Vec::new();
    // A no-switch control depends only on the source variant, and every
    // fixture serves its source variant undelayed, so each source plays once.
    // Switches play in pairs, beside any control their sources still lack, so
    // a control hears the same host load as the switches it is compared with.
    let mut controls: [Option<Render>; 3] = std::array::from_fn(|_| None);
    for [(first, first_url), (second, second_url)] in sources.as_chunks::<2>().0 {
        let first_control = controls[first.from].is_none().then_some(*first);
        let second_control =
            (controls[second.from].is_none() && second.from != first.from).then_some(*second);
        let (first_rendered, second_rendered, first_switched, second_switched) = join!(
            source_control(first_url, first_control, backend),
            source_control(second_url, second_control, backend),
            render_tiny_ring_switch(first_url, *first, backend),
            render_tiny_ring_switch(second_url, *second, backend),
        );
        for (transition, rendered) in [(first, first_rendered), (second, second_rendered)] {
            if let Some(rendered) = rendered {
                controls[transition.from] = Some(rendered);
            }
        }
        for (transition, switched) in [(first, first_switched), (second, second_switched)] {
            let control = controls[transition.from]
                .as_ref()
                .expect("every source variant control renders with its first switch");
            failures.extend(cold_switch_failures(*transition, control, &switched));
        }
    }
    assert!(
        failures.is_empty(),
        "switching to a cold target variant interrupted playback (delay_ms={COLD_TARGET_SEGMENT_DELAY_MS}): {}",
        failures.join("; "),
    );
}

/// The no-switch control of `transition`'s source variant, when it still
/// needs one.
async fn source_control(
    master_url: &url::Url,
    transition: Option<Transition>,
    backend: DecoderBackend,
) -> Option<Render> {
    match transition {
        Some(transition) => Some(render_tiny_ring_control(master_url, transition, backend).await),
        None => None,
    }
}

fn cold_switch_failures(
    transition: Transition,
    control: &Render,
    switched: &Render,
) -> Vec<String> {
    assert_eq!(
        switched.capture_frame, control.capture_frame,
        "{} switch and no-switch control must start at the same source frame",
        transition.label,
    );
    let mut failures = Vec::new();
    let control_silence = longest_silent_run(&control.samples);
    let switched_silence = longest_silent_run(&switched.samples);
    if switched_silence > control_silence.saturating_add(MAX_EXTRA_SILENT_FRAMES) {
        failures.push(format!(
            "{} inserted an audible silent gap: switched={switched_silence} frames, control={control_silence} frames; switched {}; control {}",
            transition.label,
            switched.silence_diagnostic(),
            control.silence_diagnostic(),
        ));
    }
    let control_buckets = cochlea_silent_buckets(&control.samples);
    let switched_buckets = cochlea_silent_buckets(&switched.samples);
    if switched_buckets > control_buckets {
        failures.push(format!(
            "{} produced switch-only silent buckets: switched={switched_buckets}, control={control_buckets}; switched {}; control {}",
            transition.label,
            switched.silence_diagnostic(),
            control.silence_diagnostic(),
        ));
    }
    failures
}

async fn rebuild_source(delay_ms: u64) -> (TestServerHelper, CreatedHls) {
    let server = TestServerHelper::new().await;
    let created = server
        .create_hls(target_rebuild_fixture(delay_ms))
        .await
        .unwrap_or_else(|error| panic!("create target-rebuild fixture: {error}"));
    (server, created)
}

#[kithara::fixture]
async fn slow_source() -> (TestServerHelper, CreatedHls) {
    rebuild_source(SLOW_TARGET_SEGMENT_DELAY_MS).await
}

#[kithara::fixture]
async fn target_source() -> (TestServerHelper, CreatedHls) {
    rebuild_source(TARGET_SEGMENT_DELAY_MS).await
}

#[kithara::fixture]
async fn cold_sources() -> Vec<(TestServerHelper, CreatedHls)> {
    let mut sources = Vec::new();
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
        sources.push((server, created));
    }
    sources
}

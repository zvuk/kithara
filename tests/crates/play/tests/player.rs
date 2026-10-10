use std::num::{NonZeroU32, NonZeroUsize};

use kithara::{
    assets::AssetStore,
    decode::GaplessMode,
    effects::eq::generate_log_spaced_bands,
    events::{Envelope, TrackId},
    host::{Host, HostConfig, HostOwned, HostSettings},
    output::{
        OfflineRenderError, OfflineRenderRequest, OfflineRenderer, RenderSink, RenderSinkError,
    },
    platform::{CancelScope, time::Duration},
    play::{
        BeatGrid, CrossfadeSettings, DeckMixerConfig, PlayError, PlayWorker, PlayWorkerConfig,
        PlayerEvent, PlayerStatus, ResourcePrep, TrackSettings, mock,
    },
    queue::{ActionAtItemEnd, Queue, QueueConfig, QueueError, QueueEvent, Transition},
    signal::AudioSpec,
    warp::WarpConfig,
};
use kithara_config::Config;
use kithara_test_utils::bufpool::{TestPools, pools};

fn prep() -> ResourcePrep<TestPools> {
    ResourcePrep::builder()
        .worker(PlayWorker::new(PlayWorkerConfig::builder(pools()).build()))
        .build()
}

fn config() -> QueueConfig<TestPools> {
    QueueConfig::builder()
        .prep(prep())
        .store(AssetStore::builder(pools()).build())
        .build()
}

fn host(sample_rate: NonZeroU32) -> Host<TestPools> {
    Host::new(
        HostConfig::offline(pools())
            .settings(HostSettings::builder().sample_rate(sample_rate).build())
            .build(),
    )
    .expect("offline host starts")
}

fn hosted(config: QueueConfig<TestPools>) -> (Host<TestPools>, HostOwned<Queue<TestPools>>) {
    let mut host = host(mock::SAMPLE_RATE);
    let player = host
        .insert(Queue::new(config))
        .expect("host accepts its deck");
    (host, player)
}

fn player() -> (Host<TestPools>, HostOwned<Queue<TestPools>>) {
    hosted(config())
}

#[derive(Default)]
struct Capture(Vec<f32>);

impl RenderSink for Capture {
    fn write(&mut self, samples: &[f32]) -> Result<(), RenderSinkError> {
        self.0.extend_from_slice(samples);
        Ok(())
    }
}

fn render_block(host: &mut Host<TestPools>, cursor: &mut u64) {
    let request = OfflineRenderRequest::builder()
        .spec(AudioSpec::new(2, mock::SAMPLE_RATE))
        .frames(*cursor..*cursor + 512)
        .build();
    host.render(
        &request,
        &CancelScope::new(None).token(),
        &mut Capture::default(),
    )
    .expect("the deck applies its pending batch");
    *cursor += 512;
}

#[derive(Clone, Copy)]
enum PlayerBasicScenario {
    AdvanceOnEmpty,
    EngineAccessor,
    QueueStartsEmpty,
    StartsPaused,
}

#[kithara::test]
#[case(PlayerBasicScenario::StartsPaused)]
#[case(PlayerBasicScenario::QueueStartsEmpty)]
#[case(PlayerBasicScenario::AdvanceOnEmpty)]
#[case(PlayerBasicScenario::EngineAccessor)]
fn player_basic_behaviors(#[case] scenario: PlayerBasicScenario) {
    let (mut host, player) = player();
    match scenario {
        PlayerBasicScenario::StartsPaused => {
            assert!((player.rate() - 0.0).abs() < f32::EPSILON);
            assert_eq!(player.status(), PlayerStatus::Unknown);
        }
        PlayerBasicScenario::QueueStartsEmpty => {
            assert_eq!(player.len(), 0);
        }
        PlayerBasicScenario::AdvanceOnEmpty => {
            player.next(Transition::None).expect("empty next succeeds");
            assert_eq!(player.current_index(), None);
        }
        PlayerBasicScenario::EngineAccessor => {
            assert!(!player.is_playing());
            let request = OfflineRenderRequest::builder()
                .spec(AudioSpec::new(2, mock::SAMPLE_RATE))
                .frames(0..16)
                .build();
            let mut sink = Capture::default();
            host.render(&request, &CancelScope::new(None).token(), &mut sink)
                .expect("the idle output renders silence");
            assert_eq!(sink.0.len(), 32);
            assert!(sink.0.iter().all(|sample| *sample == 0.0));
        }
    }
}

#[kithara::test]
fn player_volume_clamps() {
    let (mut host, player) = player();
    let mut cursor = 0;
    player.set_volume(2.0).expect("volume clamps");
    render_block(&mut host, &mut cursor);
    assert!((player.volume() - 1.0).abs() < f32::EPSILON);
    player.set_volume(-1.0).expect("volume clamps");
    render_block(&mut host, &mut cursor);
    assert!((player.volume() - 0.0).abs() < f32::EPSILON);
}

#[kithara::test]
fn player_muted() {
    let (mut host, player) = player();
    let mut cursor = 0;
    assert!(!player.is_muted());
    player.set_muted(true).expect("mute is accepted");
    render_block(&mut host, &mut cursor);
    assert!(player.is_muted());
}

#[kithara::test]
fn player_crossfade_duration() {
    let (_host, player) = player();
    assert!((player.crossfade_settings().duration - 1.0).abs() < f32::EPSILON);
    player
        .set_crossfade_settings(CrossfadeSettings {
            duration: 3.0,
            ..Default::default()
        })
        .expect("valid crossfade");
    assert!((player.crossfade_settings().duration - 3.0).abs() < f32::EPSILON);
}

#[kithara::test]
fn player_prefetch_duration() {
    let player = config().values();
    assert!((player.preload_lead.as_secs_f64() - 3.5).abs() < f64::from(f32::EPSILON));
    let player = QueueConfig::<TestPools>::builder()
        .prep(prep())
        .preload_lead(Duration::from_secs(8))
        .build()
        .values();
    assert!((player.preload_lead.as_secs_f64() - 8.0).abs() < f64::from(f32::EPSILON));
    assert!(Duration::try_from_secs_f64(-1.0).is_err());
    let player = QueueConfig::<TestPools>::builder()
        .prep(prep())
        .preload_lead(Duration::ZERO)
        .build()
        .values();
    assert!((player.preload_lead.as_secs_f64() - 0.0).abs() < f64::from(f32::EPSILON));
}

#[kithara::test]
fn player_events_subscribe() {
    let (mut host, player) = player();
    let mut cursor = 0;
    let mut rx = player.subscribe::<PlayerEvent>();
    player.set_volume(0.5).expect("volume is accepted");
    render_block(&mut host, &mut cursor);
    let event = rx.try_recv();
    assert!(event.is_ok());
}

#[kithara::test]
fn player_config_custom() {
    let config = QueueConfig::builder()
        .prep(
            ResourcePrep::builder()
                .worker(prep().worker)
                .gapless_mode(GaplessMode::MediaOnly)
                .warp(WarpConfig::builder().build())
                .build(),
        )
        .store(AssetStore::builder(pools()).build())
        .settings(
            kithara::queue::QueueSettings::builder()
                .crossfade(CrossfadeSettings {
                    duration: 2.0,
                    ..Default::default()
                })
                .build(),
        )
        .preload_lead(Duration::from_secs(5))
        .track(TrackSettings::builder().speed(0.5).build())
        .mixer(
            DeckMixerConfig::builder()
                .slots(NonZeroUsize::new(2).expect("two slots"))
                .build(),
        )
        .build();
    let (_host, player) = hosted(config);
    player
        .set_eq_layout(generate_log_spaced_bands(5))
        .expect("valid EQ layout");
    assert!((player.crossfade_settings().duration - 2.0).abs() < f32::EPSILON);
}

#[kithara::test]
fn a_configured_sample_rate_reaches_the_engine_it_prepares() {
    let sample_rate = NonZeroU32::new(48_000).expect("invariant: sample rate is non-zero");
    let mut host = host(sample_rate);
    let _player = host
        .insert(Queue::new(config()))
        .expect("host accepts its deck");
    let request = OfflineRenderRequest::builder()
        .spec(AudioSpec::new(2, sample_rate))
        .frames(0..1)
        .build();
    host.render(
        &request,
        &CancelScope::new(None).token(),
        &mut Capture::default(),
    )
    .expect("configured output renders");

    assert_eq!(host.output_sample_rate().output(), sample_rate.get());
}

#[kithara::test]
fn eq_band_count_tracks_a_replacement_layout_before_start() {
    let (mut host, player) = player();
    let mut cursor = 0;
    player
        .set_eq_layout(generate_log_spaced_bands(3))
        .expect("initial layout");
    render_block(&mut host, &mut cursor);
    assert_eq!(player.eq_band_count(), 3);

    player.set_eq_layout(generate_log_spaced_bands(4)).unwrap();
    render_block(&mut host, &mut cursor);
    assert_eq!(player.eq_band_count(), 4);
}

#[kithara::test]
fn player_config_builder() {
    let mut cursor = 0;
    let config = QueueConfig::builder()
        .prep(prep())
        .track(TrackSettings::builder().speed(0.5).build())
        .settings(
            kithara::queue::QueueSettings::builder()
                .crossfade(CrossfadeSettings {
                    duration: 2.5,
                    ..Default::default()
                })
                .build(),
        )
        .preload_lead(Duration::from_secs(7))
        .mixer(
            DeckMixerConfig::builder()
                .slots(NonZeroUsize::new(8).expect("eight slots"))
                .build(),
        )
        .build();
    let values = config.values();
    let mut _host = Host::new(
        HostConfig::offline(pools())
            .settings(
                HostSettings::builder()
                    .sample_rate(mock::SAMPLE_RATE)
                    .build(),
            )
            .max_deck_slots(NonZeroUsize::new(8).expect("eight slots"))
            .build(),
    )
    .expect("offline host starts");
    let player = _host
        .insert(Queue::new(config))
        .expect("host accepts its deck");
    player
        .set_eq_layout(generate_log_spaced_bands(5))
        .expect("valid EQ layout");
    render_block(&mut _host, &mut cursor);
    assert_eq!(values.mixer.slots().get(), 8);
    assert!((values.track.speed() - 0.5).abs() < f32::EPSILON);
    assert!((values.settings.crossfade().duration - 2.5).abs() < f32::EPSILON);
    assert!((values.preload_lead.as_secs_f64() - 7.0).abs() < f64::from(f32::EPSILON));
    assert_eq!(player.eq_band_count(), 5);
}

#[kithara::test(tokio)]
async fn synchronous_player_events_remain_in_order() {
    let (mut host, player) = player();
    let mut cursor = 0;
    let mut rx = player.subscribe::<PlayerEvent>();

    player.set_volume(0.5).expect("volume is accepted");
    player.set_muted(true).expect("mute is accepted");
    render_block(&mut host, &mut cursor);
    player.set_rate(2.0).expect("rate is accepted");

    let e1 = rx.try_recv();
    let e2 = rx.try_recv();
    assert!(matches!(
        e1,
        Ok(Envelope {
            event: PlayerEvent::VolumeChanged { .. },
            ..
        })
    ));
    assert!(matches!(
        e2,
        Ok(Envelope {
            event: PlayerEvent::MuteChanged { .. },
            ..
        })
    ));
    assert!(
        rx.try_recv().is_err(),
        "rate feedback must wait for the RT processor"
    );
}

#[kithara::test(tokio)]
async fn player_negative_crossfade_duration_clamped() {
    let (_host, player) = player();
    let error = player
        .set_crossfade_settings(CrossfadeSettings {
            duration: -5.0,
            ..Default::default()
        })
        .expect_err("the checked crossfade profile rejects negative duration");
    assert!(
        matches!(error, QueueError::Play(PlayError::InvalidParameter { ref name, value })
        if name == "crossfade.duration" && value == -5.0)
    );
    assert!((player.crossfade_settings().duration - 1.0).abs() < f32::EPSILON);
}

#[kithara::test]
fn auto_advance_enabled_default_and_toggle() {
    let (_host, player) = player();
    assert!(
        player.action_at_item_end() == ActionAtItemEnd::Advance,
        "default must be on"
    );
    player.set_action_at_item_end(ActionAtItemEnd::None);
    assert!(player.action_at_item_end() == ActionAtItemEnd::None);
    player.set_action_at_item_end(ActionAtItemEnd::Advance);
    assert!(player.action_at_item_end() == ActionAtItemEnd::Advance);
}

#[kithara::test]
fn auto_advance_disabled_via_config() {
    let (_host, player) = hosted(
        QueueConfig::builder()
            .prep(prep())
            .action_at_item_end(ActionAtItemEnd::None)
            .build(),
    );
    assert!(player.action_at_item_end() == ActionAtItemEnd::None);
}

#[kithara::test]
fn a_player_hands_its_sync_attachment_only_to_the_first_session_it_binds() {
    let mut first = host(mock::SAMPLE_RATE);
    let grid_id = first.id();
    let player = first
        .insert(Queue::new(config()))
        .expect("the first session binds the deck");
    let attachment = first.snapshot();
    assert_eq!(attachment.id(), grid_id);
    let mut second = host(mock::SAMPLE_RATE);
    assert_ne!(second.id(), grid_id);
    assert!(
        matches!(second.remove(&player), Err(PlayError::ForeignSession)),
        "no second owner can take the player's synchronization group"
    );
    assert!(!first.is_empty());
    assert_eq!(first.snapshot().id(), grid_id);
    first
        .remove(&player)
        .expect("the first host retains ownership");
    assert!(first.is_empty());
}

#[kithara::test]
fn host_rejects_a_player_built_for_another_sample_rate() {
    let (mut host, _player) = player();
    let request = OfflineRenderRequest::builder()
        .spec(AudioSpec::new(
            2,
            NonZeroU32::new(48_000).expect("48000 is not zero"),
        ))
        .frames(0..1)
        .build();
    let mut sink = Capture::default();
    let error = host
        .render(&request, &CancelScope::new(None).token(), &mut sink)
        .expect_err("the output owner rejects an incompatible render format");
    assert!(
        matches!(error, OfflineRenderError::SpecMismatch { expected, actual }
        if expected.sample_rate.get() == 44_100 && actual.sample_rate.get() == 48_000)
    );
    assert!(sink.0.is_empty());
}

#[kithara::test]
fn select_item_rejects_invalid_crossfade_before_index_or_engine_side_effects() {
    let (_host, player) = player();
    let err = player
        .select(
            TrackId::allocate(),
            Transition::CrossfadeWith {
                settings: CrossfadeSettings {
                    duration: -1.0,
                    ..Default::default()
                },
            },
        )
        .expect_err("invalid crossfade must be rejected");
    assert!(
        matches!(err, QueueError::Play(PlayError::InvalidParameter { ref name, value })
        if name == "crossfade.duration" && value == -1.0)
    );
    assert!(!player.is_playing());
    assert!(player.current().is_none());
}

#[kithara::test]
fn select_item_on_consumed_slot_errors_without_bookkeeping() {
    let (_host, player) = hosted(QueueConfig::builder().prep(prep()).build());
    player
        .append("https://example.com/first.mp3".to_owned())
        .expect("first entry");
    let item = player
        .append("https://example.com/consumed.mp3".to_owned())
        .expect("second entry");
    player
        .select(item, Transition::None)
        .expect_err("the entry has no loadable resource");
    let mut events = player.subscribe::<QueueEvent>();
    let result = player.select(item, Transition::None);
    assert!(result.is_err(), "selecting an emptied slot must fail");
    assert!(matches!(result, Err(QueueError::NotReady(id)) if id == item));
    assert_eq!(
        player.current_index(),
        None,
        "bookkeeping must not move on a failed select"
    );
    assert!(
        events.try_recv().is_err(),
        "a refused select announces no current item"
    );
}

#[kithara::test]
fn clearing_the_queue_drops_the_held_start_position() {
    let (_host, player) = player();
    player.seek(30.0).expect("must accept");
    assert_eq!(player.position_seconds(), Some(30.0));

    player.clear().expect("an idle deck accepts the clear");

    assert_eq!(player.position_seconds(), None);
}

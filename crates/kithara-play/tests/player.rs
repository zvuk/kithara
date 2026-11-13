use kithara_assets::AssetStore;
use kithara_command::When;
use kithara_events::{EventBus, TrackId, TryRecvError};
use kithara_platform::time::Duration;
use kithara_play::{
    DeckMixerConfig, PlayWorker, PlayWorkerConfig, Player, PlayerConfig, PlayerEvent,
    PlayerFactory, PlayerImpl, Position, ResourceConfig, ResourcePrep, ResourceSrc, Slot,
    TrackCommand, TrackFactory, TrackSettings, TrackSettingsChange, TrackStatus, mock,
};
#[cfg(all(test, target_os = "android"))]
use kithara_test_dylib as _;
use kithara_test_utils::{
    bufpool::{TestPools, pools},
    kithara,
};

fn worker() -> PlayWorker<TestPools> {
    PlayWorker::new(PlayWorkerConfig::builder(pools()).build())
}

fn player() -> PlayerImpl<TestPools> {
    PlayerFactory
        .track(PlayerConfig {
            item: TrackId::allocate(),
            slot: Some(Slot::new(0)),
            settings: TrackSettings::default(),
        })
        .expect("default track settings are valid")
}

fn rig() -> mock::DeckRig<TestPools> {
    mock::DeckRig::new(DeckMixerConfig::default()).expect("deck scope opens")
}

#[kithara::test]
fn an_empty_deck_rig_runs_its_mixer_block() {
    assert!(
        rig()
            .block(kithara_signal::SessionFrame::new(0), 0.0)
            .expect("mock block")
            .is_empty()
    );
}

fn prepared(prep: &ResourcePrep<TestPools>) -> ResourceConfig<TestPools> {
    let config = ResourceConfig::for_src(
        ResourceSrc::parse("https://example.com/song.mp3").expect("valid fixture URI"),
    )
    .store(AssetStore::builder(pools()).build())
    .build();
    prep.prepare(config, &mock::output(None).get())
        .expect("unbound preparation succeeds")
}

#[kithara::test]
fn player_pause_without_active_slot_keeps_rate_zero() {
    let mut player = player();
    rig()
        .with_outbox(|out| player.apply(TrackCommand::Pause { at: When::Next }, out))
        .expect("live deck scope")
        .expect("idle pause succeeds");
    assert_eq!(player.snapshot().status, TrackStatus::Idle);
}

#[kithara::test]
fn player_config_sets_capacity_for_a_new_event_bus() {
    let player = ResourcePrep::builder()
        .worker(worker())
        .bus(EventBus::new(2))
        .build();
    let prepared = prepared(&player);
    let bus = prepared.bus().expect("preparation supplies the scoped bus");
    let mut rx = player.bus.subscribe::<PlayerEvent>();

    bus.publish(PlayerEvent::VolumeChanged { volume: 0.1 });
    bus.publish(PlayerEvent::VolumeChanged { volume: 0.2 });
    bus.publish(PlayerEvent::VolumeChanged { volume: 0.3 });

    assert!(matches!(rx.try_recv(), Err(TryRecvError::Lagged(1))));
}

#[kithara::test]
fn injected_event_bus_keeps_its_identity_and_capacity() {
    let bus = EventBus::new(1);
    let bus_id = bus.id();
    let player = ResourcePrep::builder().worker(worker()).bus(bus).build();
    let prepared = prepared(&player);
    let bus = prepared.bus().expect("preparation supplies the scoped bus");
    let mut rx = player.bus.subscribe::<PlayerEvent>();

    assert_eq!(player.bus.id(), bus_id);
    bus.publish(PlayerEvent::VolumeChanged { volume: 0.1 });
    bus.publish(PlayerEvent::VolumeChanged { volume: 0.2 });
    assert!(matches!(rx.try_recv(), Err(TryRecvError::Lagged(1))));
}

#[kithara::test]
fn position_seconds_idle_is_none() {
    let player = player();
    let snapshot = player.snapshot();
    assert_eq!(snapshot.position, Position::ZERO);
    assert!(snapshot.duration.is_none());
    assert!(!matches!(snapshot.status, TrackStatus::Playing { .. }));
    assert!(snapshot.abr.is_none());
    assert!(snapshot.mark.is_none());
    assert!(!snapshot.attached);
    assert!(!snapshot.pending_lane);
}

#[kithara::test]
fn set_rate_without_rt_does_not_emit_rate_changed() {
    let mut player = player();
    let mut rig = rig();
    let sent = rig
        .with_outbox(|out| {
            player.apply(
                TrackCommand::Configure(TrackSettingsChange::Speed(2.0), When::Next),
                out,
            )
        })
        .expect("live deck scope")
        .expect("idle speed change succeeds");
    assert!(sent.is_none(), "idle speed changes send no RT command");
    assert_eq!(player.snapshot().speed, 2.0);
    assert!(rig.events.drain().next().is_none());
}

#[kithara::test]
fn player_keeps_explicit_worker_and_shared_pools() {
    let worker = worker();
    let player = ResourcePrep::builder().worker(worker.clone()).build();
    assert!(std::ptr::eq(player.worker.pools(), worker.pools(),));
}

#[kithara::test]
fn seek_seconds_without_slot_holds_the_start_position() {
    let mut player = player();
    let target = Duration::from_secs(12);
    rig()
        .with_outbox(|out| player.apply(TrackCommand::Seek { to: target }, out))
        .expect("live deck scope")
        .expect("must accept");

    let landed_at = player.snapshot().position;
    assert!(target == Duration::from_secs(12) && landed_at == target);
    assert_eq!(player.snapshot().position.as_secs_f64(), 12.0);
}

#[kithara::test]
fn held_start_position_keeps_the_latest_target() {
    let mut player = player();
    let mut rig = rig();
    rig.with_outbox(|out| player.apply(TrackCommand::Seek { to: Position::ZERO }, out))
        .expect("live deck scope")
        .expect("must accept");
    rig.with_outbox(|out| {
        player.apply(
            TrackCommand::Seek {
                to: Position::from_secs(30),
            },
            out,
        )
    })
    .expect("live deck scope")
    .expect("must accept");

    assert_eq!(player.snapshot().position.as_secs_f64(), 30.0);
}

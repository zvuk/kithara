use std::num::NonZeroU32;

use kithara::{
    self,
    audio::ConsumerWakeMode,
    events::EventBus,
    host::{Host, HostConfig, HostOwned, testing::HostProbe},
    platform::sync::Arc,
    play::{
        Cmd, EngineConfig, EngineImpl, PlayError, PlayWorker, PlayWorkerConfig, PlayerConfig,
        PlayerImpl, Reply, SessionDispatcher, SessionDuckingMode, SlotId,
    },
    warp::{BeatGrid, BeatGridId},
};
use kithara_integration_tests::test_defaults::Consts as Shared;

use crate::bufpool_ext::{TestPools, pools};

struct FixtureSession;

impl SessionDispatcher<TestPools> for FixtureSession {
    fn exec(&self, _cmd: Cmd<TestPools>) -> Result<Reply, PlayError> {
        Ok(Reply::Ok)
    }

    fn consumer_wake_mode(&self) -> ConsumerWakeMode {
        ConsumerWakeMode::RealtimeDeferred
    }
}

fn slot_id(value: u64) -> SlotId {
    SlotId::new(value)
}

fn make_engine() -> EngineImpl<TestPools> {
    EngineImpl::new(
        EngineConfig::builder()
            .sample_rate(Shared::NON_ZERO_SAMPLE_RATE)
            .grid_id(BeatGridId::allocate().expect("fixture grid id"))
            .session(Arc::new(FixtureSession))
            .pools(pools())
            .build(),
        EventBus::default(),
    )
}

fn insert_player(host: &mut Host<TestPools>) -> HostOwned<PlayerImpl<TestPools>> {
    let player = PlayerImpl::new(
        PlayerConfig::builder()
            .sample_rate(host.requested_sample_rate())
            .worker(PlayWorker::new(PlayWorkerConfig::builder(pools()).build()))
            .build(),
    );
    let instance_id = player.id();
    let owner = host.insert(player).expect("insert fixture player instance");
    assert_eq!(owner.id(), instance_id);
    owner
}

#[derive(Clone, Copy)]
enum EngineInitialScenario {
    ActiveSlotsEmpty,
    NotRunning,
    SlotState,
}

#[derive(Clone, Copy)]
enum NotRunningErrorScenario {
    AllocateSlot,
    ReleaseSlot,
    Stop,
}

#[kithara::test]
fn engine_config_defaults() {
    let engine = make_engine();
    assert_eq!(engine.max_slots(), 4);
    assert_eq!(engine.master_sample_rate(), 44100);
}

#[kithara::test]
fn engine_config_builder() {
    let config = EngineConfig::builder()
        .grid_id(BeatGridId::allocate().expect("fixture grid id"))
        .session(Arc::new(FixtureSession))
        .max_slots(8)
        .sample_rate(NonZeroU32::new(48_000).expect("fixture sample rate is non-zero"))
        .channels(1)
        .eq_layout(kithara::play::effects::eq::generate_log_spaced_bands(5))
        .pools(pools())
        .build();
    let engine = EngineImpl::new(config, EventBus::default());
    assert!(!engine.is_running());
    assert_eq!(engine.max_slots(), 8);
    assert_eq!(engine.master_sample_rate(), 48000);
}

#[kithara::test]
#[case(EngineInitialScenario::NotRunning)]
#[case(EngineInitialScenario::SlotState)]
#[case(EngineInitialScenario::ActiveSlotsEmpty)]
fn engine_initial_state(#[case] scenario: EngineInitialScenario) {
    let engine = make_engine();
    match scenario {
        EngineInitialScenario::NotRunning => assert!(!engine.is_running()),
        EngineInitialScenario::SlotState => {
            assert_eq!(engine.active_slots().len(), 0);
            assert_eq!(engine.max_slots(), 4);
        }
        EngineInitialScenario::ActiveSlotsEmpty => assert!(engine.active_slots().is_empty()),
    }
}

#[kithara::test]
fn engine_subscribe_works() {
    let engine = make_engine();
    let _rx = engine.subscribe();
}

#[kithara::test]
fn engine_master_volume_default() {
    let engine = make_engine();
    assert!((engine.master_volume() - 1.0).abs() < f32::EPSILON);
}

#[kithara::test]
#[case(NotRunningErrorScenario::Stop)]
#[case(NotRunningErrorScenario::AllocateSlot)]
#[case(NotRunningErrorScenario::ReleaseSlot)]
fn engine_not_running_operations_return_error(#[case] scenario: NotRunningErrorScenario) {
    let engine = make_engine();
    let err = match scenario {
        NotRunningErrorScenario::Stop => engine.stop().unwrap_err(),
        NotRunningErrorScenario::AllocateSlot => engine.allocate_slot().unwrap_err(),
        NotRunningErrorScenario::ReleaseSlot => engine.release_slot(slot_id(99)).unwrap_err(),
    };
    assert!(matches!(err, PlayError::EngineNotRunning));
}

#[kithara::test]
fn engine_session_ducking_roundtrip() {
    let host: Host<TestPools> =
        Host::new(HostConfig::builder().build()).expect("create fixture host");
    host.set_ducking(SessionDuckingMode::Soft)
        .expect("set soft ducking");
    assert_eq!(
        host.ducking().expect("read soft ducking"),
        SessionDuckingMode::Soft
    );
    host.set_ducking(SessionDuckingMode::Hard)
        .expect("set hard ducking");
    assert_eq!(
        host.ducking().expect("read hard ducking"),
        SessionDuckingMode::Hard
    );
    host.set_ducking(SessionDuckingMode::Off)
        .expect("disable ducking");
    assert_eq!(
        host.ducking().expect("read disabled ducking"),
        SessionDuckingMode::Off
    );
}

#[kithara::test]
fn injected_engine_instances_share_session_ducking() {
    let mut host = Host::new(HostConfig::builder().build()).expect("create fixture host");
    let a = insert_player(&mut host);
    let b = insert_player(&mut host);

    host.set_ducking(SessionDuckingMode::Soft)
        .expect("set shared ducking");
    assert_eq!(
        host.ducking().expect("read shared ducking"),
        SessionDuckingMode::Soft
    );

    host.set_ducking(SessionDuckingMode::Off)
        .expect("disable shared ducking");
    assert_eq!(
        host.ducking().expect("read disabled ducking"),
        SessionDuckingMode::Off
    );

    host.remove(&a).expect("remove first fixture player");
    host.remove(&b).expect("remove second fixture player");
}

#[kithara::test]
fn foreign_host_cannot_close_owned_player() {
    let mut owner_host = Host::new(HostConfig::builder().build()).expect("create owner host");
    let mut foreign_host = Host::new(HostConfig::builder().build()).expect("create foreign host");
    let player = insert_player(&mut owner_host);

    let error = foreign_host
        .remove(&player)
        .expect_err("foreign host must reject the player before closing it");
    assert!(matches!(error, PlayError::ForeignSession));
    assert!(
        !player.is_closed(),
        "foreign remove must not close the player"
    );

    owner_host
        .remove(&player)
        .expect("owning host removes its player");
    assert!(player.is_closed());
}

#[kithara::test]
fn dropping_host_invalidates_retained_player_control() {
    let player = {
        let mut host = Host::new(HostConfig::builder().build()).expect("create fixture host");
        insert_player(&mut host)
    };

    assert!(
        player.is_closed(),
        "dropping the canonical host must invalidate retained controls"
    );
}

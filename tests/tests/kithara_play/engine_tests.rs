use kithara::{
    self,
    bufpool::PcmPool,
    events::EventBus,
    play::{
        Cmd, EngineConfig, EngineImpl, PlayError, Reply, SessionDuckingMode, SessionHandle, SlotId,
    },
};

fn slot_id(value: u64) -> SlotId {
    SlotId::new(value)
}

fn make_engine() -> EngineImpl {
    EngineImpl::new(
        EngineConfig::builder().pcm_pool(PcmPool::default()).build(),
        EventBus::default(),
    )
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
        .max_slots(8)
        .sample_rate(48000)
        .channels(1)
        .eq_layout(kithara::audio::generate_log_spaced_bands(5))
        .pcm_pool(PcmPool::default())
        .build();
    let engine = EngineImpl::new(config, EventBus::default());
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
fn engine_master_sample_rate_returns_config_when_stopped() {
    let config = EngineConfig::builder()
        .sample_rate(48000)
        .pcm_pool(PcmPool::default())
        .build();
    let engine = EngineImpl::new(config, EventBus::default());
    assert_eq!(engine.master_sample_rate(), 48000);
}

fn set_ducking(session: &SessionHandle, mode: SessionDuckingMode) {
    session
        .exec_ok(Cmd::SetSessionDucking { mode })
        .expect("set ducking");
}

fn ducking(session: &SessionHandle) -> SessionDuckingMode {
    match session.exec_ok(Cmd::SessionDucking).expect("ducking") {
        Reply::SessionDucking(mode) => mode,
        _ => panic!("unexpected ducking reply"),
    }
}

#[kithara::test]
fn engine_session_ducking_roundtrip() {
    let session = SessionHandle::spawn_native();
    set_ducking(&session, SessionDuckingMode::Soft);
    assert_eq!(ducking(&session), SessionDuckingMode::Soft);
    set_ducking(&session, SessionDuckingMode::Hard);
    assert_eq!(ducking(&session), SessionDuckingMode::Hard);
    set_ducking(&session, SessionDuckingMode::Off);
    assert_eq!(ducking(&session), SessionDuckingMode::Off);
}

#[kithara::test]
fn injected_engine_instances_share_session_ducking() {
    let session = SessionHandle::spawn_native();
    let config = EngineConfig::builder()
        .session(session.dispatcher())
        .pcm_pool(PcmPool::default())
        .build();
    let _a = EngineImpl::new(config.clone(), EventBus::default());
    let _b = EngineImpl::new(config, EventBus::default());

    set_ducking(&session, SessionDuckingMode::Soft);
    assert_eq!(ducking(&session), SessionDuckingMode::Soft);

    set_ducking(&session, SessionDuckingMode::Off);
    assert_eq!(ducking(&session), SessionDuckingMode::Off);
}

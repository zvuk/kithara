use std::sync::{Mutex as StdMutex, OnceLock};

use kithara::{
    self,
    events::EventBus,
    play::{EngineConfig, EngineImpl, PlayError, SessionDuckingMode, SlotId},
};

fn slot_id(value: u64) -> SlotId {
    SlotId::new(value)
}

fn make_engine() -> EngineImpl {
    EngineImpl::new(EngineConfig::default(), EventBus::default())
}

fn session_ducking_lock() -> &'static StdMutex<()> {
    static LOCK: OnceLock<StdMutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| StdMutex::new(()))
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
    let config = EngineConfig::default();
    assert_eq!(config.channels, 2);
    assert_eq!(config.eq_layout.len(), 10);
    assert_eq!(config.max_slots, 4);
    assert_eq!(config.sample_rate, 44100);
}

#[kithara::test]
fn engine_config_builder() {
    let config = EngineConfig::builder()
        .max_slots(8)
        .sample_rate(48000)
        .channels(1)
        .eq_layout(kithara::audio::generate_log_spaced_bands(5))
        .build();
    assert_eq!(config.max_slots, 8);
    assert_eq!(config.sample_rate, 48000);
    assert_eq!(config.channels, 1);
    assert_eq!(config.eq_layout.len(), 5);
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
    let config = EngineConfig::builder().sample_rate(48000).build();
    let engine = EngineImpl::new(config, EventBus::default());
    assert_eq!(engine.master_sample_rate(), 48000);
}

#[kithara::test]
fn engine_session_ducking_roundtrip() {
    let _lock = session_ducking_lock().lock().unwrap();
    EngineImpl::set_session_ducking(SessionDuckingMode::Soft).unwrap();
    assert_eq!(EngineImpl::session_ducking(), SessionDuckingMode::Soft);
    EngineImpl::set_session_ducking(SessionDuckingMode::Hard).unwrap();
    assert_eq!(EngineImpl::session_ducking(), SessionDuckingMode::Hard);
    EngineImpl::set_session_ducking(SessionDuckingMode::Off).unwrap();
    assert_eq!(EngineImpl::session_ducking(), SessionDuckingMode::Off);
}

#[kithara::test]
fn engine_instances_share_session_ducking() {
    let _lock = session_ducking_lock().lock().unwrap();
    let _a = make_engine();
    let _b = make_engine();

    EngineImpl::set_session_ducking(SessionDuckingMode::Soft).unwrap();
    assert_eq!(EngineImpl::session_ducking(), SessionDuckingMode::Soft);

    EngineImpl::set_session_ducking(SessionDuckingMode::Off).unwrap();
    assert_eq!(EngineImpl::session_ducking(), SessionDuckingMode::Off);
}

use kithara::{
    self,
    events::EventBus,
    play::{EngineConfig, EngineImpl, PlayError},
};
use kithara_integration_tests::offline::OfflineSession;

#[derive(Clone, Copy, Debug)]
enum Backend {
    Cpal,
    Offline,
}

fn engine_config(backend: Backend) -> EngineConfig {
    engine_config_with_max_slots(backend, 4)
}

fn engine_config_with_max_slots(backend: Backend, max_slots: usize) -> EngineConfig {
    let builder = EngineConfig::builder().max_slots(max_slots);
    if matches!(backend, Backend::Offline) {
        builder.session(OfflineSession::arc_auto()).build()
    } else {
        builder.build()
    }
}

#[kithara::test]
#[case::cpal(Backend::Cpal)]
#[case::offline(Backend::Offline)]
fn engine_start_stop_roundtrip(#[case] backend: Backend) {
    let engine = EngineImpl::new(engine_config(backend), EventBus::default());
    engine.start().unwrap();
    assert!(engine.is_running());
    engine.stop().unwrap();
    assert!(!engine.is_running());
}

#[kithara::test]
#[case::cpal(Backend::Cpal)]
#[case::offline(Backend::Offline)]
fn engine_allocate_and_release_slot(#[case] backend: Backend) {
    let engine = EngineImpl::new(engine_config(backend), EventBus::default());
    engine.start().unwrap();

    let slot_id = engine.allocate_slot().unwrap();
    assert_eq!(engine.active_slots().len(), 1);
    assert!(engine.active_slots().contains(&slot_id));

    engine.release_slot(slot_id).unwrap();
    assert_eq!(engine.active_slots().len(), 0);

    engine.stop().unwrap();
}

#[kithara::test]
#[case::cpal(Backend::Cpal)]
#[case::offline(Backend::Offline)]
fn engine_arena_full_error(#[case] backend: Backend) {
    let config = engine_config_with_max_slots(backend, 1);
    let engine = EngineImpl::new(config, EventBus::default());
    engine.start().unwrap();

    let _slot1 = engine.allocate_slot().unwrap();
    let result = engine.allocate_slot();
    assert!(matches!(result, Err(PlayError::ArenaFull)));

    engine.stop().unwrap();
}

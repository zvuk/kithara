use kithara_events::EventBus;
use kithara_integration_tests::offline::OfflineSession;
use kithara_play::{PlayError, test_helpers::engine::*};
use kithara_test_utils::kithara;

#[derive(Clone, Copy, Debug)]
enum Backend {
    Cpal,
    Offline,
}

fn engine_config(backend: Backend) -> EngineConfig {
    let mut config = EngineConfig::default();
    if matches!(backend, Backend::Offline) {
        config.session = Some(OfflineSession::arc_auto());
    }
    config
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
    assert_eq!(engine.slot_count(), 1);
    assert!(engine.active_slots().contains(&slot_id));

    engine.release_slot(slot_id).unwrap();
    assert_eq!(engine.slot_count(), 0);

    engine.stop().unwrap();
}

#[kithara::test]
#[case::cpal(Backend::Cpal)]
#[case::offline(Backend::Offline)]
fn engine_arena_full_error(#[case] backend: Backend) {
    let mut config = engine_config(backend);
    config.max_slots = 1;
    let engine = EngineImpl::new(config, EventBus::default());
    engine.start().unwrap();

    let _slot1 = engine.allocate_slot().unwrap();
    let result = engine.allocate_slot();
    assert!(matches!(result, Err(PlayError::ArenaFull)));

    engine.stop().unwrap();
}

//! Engine lifecycle tests параметризованные по backend (`cpal | offline`).
//!
//! Запускают `EngineImpl::start()`/`allocate_slot`/`stop` через
//! настоящую сессию. `cpal`-вариант поднимает реальное audio-устройство
//! и поэтому остаётся в `suite_e2e`; `offline`-вариант форсит
//! `init_offline_backend()` перед `start()` — тоже здесь, чтобы оба
//! делителя одного singleton'а жили рядом и не путались с обычными
//! `engine_tests.rs` юнит-тестами в `suite_light`.

use std::sync::Once;

use kithara_events::EventBus;
use kithara_play::{
    PlayError,
    internal::{engine::*, init_offline_backend},
};
use kithara_test_utils::kithara;

#[derive(Clone, Copy, Debug)]
enum Backend {
    Cpal,
    Offline,
}

static INIT_OFFLINE: Once = Once::new();

fn prepare_backend(backend: Backend) {
    if matches!(backend, Backend::Offline) {
        INIT_OFFLINE.call_once(init_offline_backend);
    }
}

fn make_engine() -> EngineImpl {
    EngineImpl::new(EngineConfig::default(), EventBus::default())
}

#[kithara::test]
#[case::cpal(Backend::Cpal)]
#[case::offline(Backend::Offline)]
fn engine_start_stop_roundtrip(#[case] backend: Backend) {
    prepare_backend(backend);
    let engine = make_engine();
    engine.start().unwrap();
    assert!(engine.is_running());
    engine.stop().unwrap();
    assert!(!engine.is_running());
}

#[kithara::test]
#[case::cpal(Backend::Cpal)]
#[case::offline(Backend::Offline)]
fn engine_allocate_and_release_slot(#[case] backend: Backend) {
    prepare_backend(backend);
    let engine = make_engine();
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
    prepare_backend(backend);
    let config = EngineConfig {
        max_slots: 1,
        ..Default::default()
    };
    let engine = EngineImpl::new(config, EventBus::default());
    engine.start().unwrap();

    let _slot1 = engine.allocate_slot().unwrap();
    let result = engine.allocate_slot();
    assert!(matches!(result, Err(PlayError::ArenaFull)));

    engine.stop().unwrap();
}

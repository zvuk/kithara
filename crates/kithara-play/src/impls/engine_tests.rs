use std::sync::Arc;

use rstest::rstest;

use super::*;

fn make_engine() -> EngineImpl {
    EngineImpl::new(EngineConfig::default())
}

#[derive(Clone, Copy)]
enum EngineInitialScenario {
    ActiveSlotsEmpty,
    NotCrossfading,
    NotRunning,
    SlotState,
}

#[derive(Clone, Copy)]
enum NotRunningErrorScenario {
    AllocateSlot,
    ReleaseSlot,
    Stop,
}

#[test]
fn engine_config_defaults() {
    let config = EngineConfig::default();
    assert_eq!(config.channels, 2);
    assert_eq!(config.eq_bands, 10);
    assert_eq!(config.max_slots, 4);
    assert_eq!(config.sample_rate, 44100);
}

#[test]
fn engine_config_default_thread_pool_is_none() {
    let config = EngineConfig::default();
    assert!(config.thread_pool.is_none());
}

#[test]
fn engine_config_builder() {
    let pool = ThreadPool::with_num_threads(1).unwrap();
    let config = EngineConfig::default()
        .with_max_slots(8)
        .with_sample_rate(48000)
        .with_channels(1)
        .with_eq_bands(5)
        .with_thread_pool(pool);
    assert_eq!(config.max_slots, 8);
    assert_eq!(config.sample_rate, 48000);
    assert_eq!(config.channels, 1);
    assert!(config.thread_pool.is_some());
    assert_eq!(config.eq_bands, 5);
}

#[rstest]
#[case(EngineInitialScenario::NotRunning)]
#[case(EngineInitialScenario::SlotState)]
#[case(EngineInitialScenario::ActiveSlotsEmpty)]
#[case(EngineInitialScenario::NotCrossfading)]
fn engine_initial_state(#[case] scenario: EngineInitialScenario) {
    let engine = make_engine();
    match scenario {
        EngineInitialScenario::NotRunning => assert!(!engine.is_running()),
        EngineInitialScenario::SlotState => {
            assert_eq!(engine.slot_count(), 0);
            assert_eq!(engine.max_slots(), 4);
        }
        EngineInitialScenario::ActiveSlotsEmpty => assert!(engine.active_slots().is_empty()),
        EngineInitialScenario::NotCrossfading => assert!(!engine.is_crossfading()),
    }
}

#[test]
fn engine_subscribe_works() {
    let engine = make_engine();
    let _rx = engine.subscribe();
}

#[test]
fn engine_master_volume_default() {
    let engine = make_engine();
    assert!((engine.master_volume() - 1.0).abs() < f32::EPSILON);
}

#[test]
fn engine_set_master_volume() {
    let engine = make_engine();
    engine.set_master_volume(0.5);
    assert!((engine.master_volume() - 0.5).abs() < f32::EPSILON);
}

#[test]
fn engine_set_master_volume_clamps() {
    let engine = make_engine();
    engine.set_master_volume(2.0);
    assert!((engine.master_volume() - 1.0).abs() < f32::EPSILON);
    engine.set_master_volume(-0.5);
    assert!(engine.master_volume().abs() < f32::EPSILON);
}

#[test]
fn engine_set_master_volume_emits_event() {
    let engine = make_engine();
    let mut rx = engine.subscribe();
    engine.set_master_volume(0.75);
    let event = rx.try_recv().unwrap();
    assert!(
        matches!(event, EngineEvent::MasterVolumeChanged { volume } if (volume - 0.75).abs() < f32::EPSILON)
    );
}

#[rstest]
#[case(NotRunningErrorScenario::Stop)]
#[case(NotRunningErrorScenario::AllocateSlot)]
#[case(NotRunningErrorScenario::ReleaseSlot)]
fn engine_not_running_operations_return_error(#[case] scenario: NotRunningErrorScenario) {
    let engine = make_engine();
    let err = match scenario {
        NotRunningErrorScenario::Stop => engine.stop().unwrap_err(),
        NotRunningErrorScenario::AllocateSlot => engine.allocate_slot().unwrap_err(),
        NotRunningErrorScenario::ReleaseSlot => engine.release_slot(SlotId(99)).unwrap_err(),
    };
    assert!(matches!(err, PlayError::EngineNotRunning));
}

#[test]
fn engine_crossfade_stub_returns_not_ready() {
    let engine = make_engine();
    let err = engine
        .crossfade(SlotId(1), SlotId(2), CrossfadeConfig::default())
        .unwrap_err();
    assert!(matches!(err, PlayError::NotReady));
}

#[test]
fn engine_cancel_crossfade_stub_returns_no_crossfade() {
    let engine = make_engine();
    let err = engine.cancel_crossfade().unwrap_err();
    assert!(matches!(err, PlayError::NoCrossfade));
}

#[test]
fn engine_config_thread_pool_used_by_engine() {
    let pool = ThreadPool::with_num_threads(1).unwrap();
    let config = EngineConfig::default().with_thread_pool(pool);
    // Engine should accept a custom thread pool without panicking.
    let engine = EngineImpl::new(config);
    assert!(!engine.is_running());
}

#[test]
fn engine_master_sample_rate_returns_config_when_stopped() {
    let config = EngineConfig {
        sample_rate: 48000,
        ..Default::default()
    };
    let engine = EngineImpl::new(config);
    assert_eq!(engine.master_sample_rate(), 48000);
}

#[test]
fn engine_master_channels_returns_config() {
    let engine = make_engine();
    assert_eq!(engine.master_channels(), 2);
}

#[test]
fn engine_call_routes_replies_to_each_caller() {
    let engine = Arc::new(make_engine());
    let iterations = 400;

    let engine_a = Arc::clone(&engine);
    let a = std::thread::spawn(move || {
        for _ in 0..iterations {
            let reply = engine_a
                .call(Cmd::QuerySampleRate)
                .expect("query sample rate call should return reply");
            assert!(matches!(reply, Reply::SampleRate(44100)));
        }
    });

    let engine_b = Arc::clone(&engine);
    let b = std::thread::spawn(move || {
        for _ in 0..iterations {
            let reply = engine_b
                .call(Cmd::SetMasterVolume { volume: 0.5 })
                .expect("set master volume call should return reply");
            assert!(matches!(reply, Reply::Err(_)));
        }
    });

    a.join().expect("query thread should not panic");
    b.join().expect("set volume thread should not panic");
}

// Tests that require actual audio hardware should be marked #[ignore].
// They are run explicitly during local development or on hardware-capable CI.

#[test]
#[ignore = "requires audio hardware"]
fn engine_start_stop_roundtrip() {
    let engine = make_engine();
    engine.start().unwrap();
    assert!(engine.is_running());
    engine.stop().unwrap();
    assert!(!engine.is_running());
}

#[test]
#[ignore = "requires audio hardware"]
fn engine_allocate_and_release_slot() {
    let engine = make_engine();
    engine.start().unwrap();

    let slot_id = engine.allocate_slot().unwrap();
    assert_eq!(engine.slot_count(), 1);
    assert!(engine.active_slots().contains(&slot_id));

    engine.release_slot(slot_id).unwrap();
    assert_eq!(engine.slot_count(), 0);

    engine.stop().unwrap();
}

#[test]
#[ignore = "requires audio hardware"]
fn engine_arena_full_error() {
    let config = EngineConfig {
        max_slots: 1,
        ..Default::default()
    };
    let engine = EngineImpl::new(config);
    engine.start().unwrap();

    let _slot1 = engine.allocate_slot().unwrap();
    let result = engine.allocate_slot();
    assert!(matches!(result, Err(PlayError::ArenaFull)));

    engine.stop().unwrap();
}

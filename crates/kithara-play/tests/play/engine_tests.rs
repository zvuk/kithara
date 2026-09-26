use std::num::{NonZeroU32, NonZeroUsize};

use kithara_audio::ConsumerWakeMode;
use kithara_events::EventBus;
use kithara_platform::sync::Arc;
use kithara_play::{
    Cmd, EngineConfig, EngineImpl, PlayError, Reply, SessionBinding, SessionDispatcher, SlotId,
};
use kithara_test_utils::{
    bufpool::{TestPools, pools},
    kithara,
};
use kithara_warp::BeatGridId;

use crate::support::SAMPLE_RATE;

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

fn response_budget() -> NonZeroUsize {
    NonZeroUsize::new(448).expect("fixture response budget is non-zero")
}

fn make_engine() -> EngineImpl<TestPools> {
    EngineImpl::new(
        EngineConfig::builder()
            .sample_rate(SAMPLE_RATE)
            .grid_id(BeatGridId::allocate().expect("fixture grid id"))
            .session(SessionBinding::new(Arc::new(FixtureSession), SAMPLE_RATE))
            .pools(pools())
            .response_budget_frames(response_budget())
            .build(),
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
        .grid_id(BeatGridId::allocate().expect("fixture grid id"))
        .session(SessionBinding::new(Arc::new(FixtureSession), SAMPLE_RATE))
        .max_slots(8)
        .sample_rate(NonZeroU32::new(48_000).expect("fixture sample rate is non-zero"))
        .channels(1)
        .eq_layout(kithara_effects::eq::generate_log_spaced_bands(5))
        .pools(pools())
        .response_budget_frames(response_budget())
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
    let _rx = engine.subscribe::<kithara_play::EngineEvent>();
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
        .grid_id(BeatGridId::allocate().expect("fixture grid id"))
        .session(SessionBinding::new(Arc::new(FixtureSession), SAMPLE_RATE))
        .sample_rate(NonZeroU32::new(48_000).expect("fixture sample rate is non-zero"))
        .pools(pools())
        .response_budget_frames(response_budget())
        .build();
    let engine = EngineImpl::new(config, EventBus::default());
    assert_eq!(engine.master_sample_rate(), 48000);
}

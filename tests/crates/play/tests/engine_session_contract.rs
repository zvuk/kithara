//! The engine lifecycle contract is the same whatever session drives the graph.
//! The caller supplies an EngineImpl; each fixture decides which session and
//! backend it uses, and therefore which suite owns the test.
use kithara::play::{EngineImpl, PlayError};

use crate::bufpool_ext::TestPools;

pub(super) fn start_stop_roundtrip(engine: &EngineImpl<TestPools>) {
    engine.start().unwrap();
    assert!(engine.is_running());
    engine.stop().unwrap();
    assert!(!engine.is_running());
}

pub(super) fn allocate_and_release_slot(engine: &EngineImpl<TestPools>) {
    engine.start().unwrap();

    let slot_id = engine.allocate_slot().unwrap();
    assert_eq!(engine.active_slots().len(), 1);
    assert!(engine.active_slots().contains(&slot_id));

    engine.release_slot(slot_id).unwrap();
    assert_eq!(engine.active_slots().len(), 0);

    engine.stop().unwrap();
}

/// The supplied engine must set `max_slots` to 1.
pub(super) fn arena_full_error(engine: &EngineImpl<TestPools>) {
    engine.start().unwrap();

    let _slot = engine.allocate_slot().unwrap();
    let result = engine.allocate_slot();
    assert!(matches!(result, Err(PlayError::ArenaFull)));

    engine.stop().unwrap();
}

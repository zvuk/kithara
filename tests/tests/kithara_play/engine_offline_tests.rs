//! An injected offline session dispatcher replaces the cpal output stream, so
//! this half asks nothing of the machine and belongs in the ordinary gate
//! rather than a lane.
use std::num::NonZeroUsize;

use kithara::{
    events::EventBus,
    play::{EngineConfig, EngineImpl},
    warp::BeatGridId,
};
use kithara_integration_tests::offline::OfflineSession;

use super::engine_session_contract as contract;
use crate::bufpool_ext::{TestPools, pools};

fn engine(max_slots: usize) -> EngineImpl<TestPools> {
    EngineImpl::new(
        EngineConfig::builder()
            .grid_id(BeatGridId::allocate().expect("offline engine grid id"))
            .max_slots(max_slots)
            .response_budget_frames(
                NonZeroUsize::new(448).expect("fixture response budget is non-zero"),
            )
            .render_quantum_frames(
                NonZeroUsize::new(32).expect("fixture render quantum is non-zero"),
            )
            .pools(pools())
            .session(OfflineSession::arc_auto())
            .build(),
        EventBus::default(),
    )
}

#[kithara::test]
fn engine_start_stop_roundtrip() {
    contract::start_stop_roundtrip(&engine(4));
}

#[kithara::test]
fn engine_allocate_and_release_slot() {
    contract::allocate_and_release_slot(&engine(4));
}

#[kithara::test]
fn engine_arena_full_error() {
    contract::arena_full_error(&engine(1));
}

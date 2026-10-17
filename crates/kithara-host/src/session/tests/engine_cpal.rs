//! The deck lifecycle runs through the Host graph with an offline backend.
use kithara_events::EventBus;
use kithara_test_utils::kithara;
use kithara_warp::BeatGridId;

use super::graph::GraphSession;

/// A deck the session takes runs the offline stream until the session hands it
/// back, and the last deck takes the stream with it.
#[kithara::test]
fn a_deck_runs_the_offline_stream_from_attach_to_detach() {
    let mut graph = GraphSession::new(crate::session::offline::mock::start);
    let grid_id = BeatGridId::allocate().expect("fixture grid id");
    assert!(graph.install(grid_id, EventBus::default()).is_ok());
    assert!(
        graph.ctx_mut().is_some(),
        "an attached deck runs the stream"
    );

    assert!(graph.remove(grid_id).is_ok());
    assert!(
        graph.ctx_mut().is_none(),
        "the last deck the session hands back takes the stream with it"
    );
}

//! The deck lifecycle runs through the same Host graph with a cpal backend.
use firewheel::{FirewheelContext, cpal::CpalStream};
use kithara_events::EventBus;
use kithara_test_utils::kithara;
use kithara_warp::BeatGridId;

use super::graph::GraphSession;

fn start_stream(ctx: &mut FirewheelContext, sample_rate: u32) -> Result<CpalStream, String> {
    let config = firewheel::cpal::CpalConfig {
        output: firewheel::cpal::CpalOutputConfig {
            desired_sample_rate: Some(sample_rate),
            ..Default::default()
        },
        ..Default::default()
    };
    CpalStream::new(ctx, config).map_err(|error| error.to_string())
}

/// A deck the session takes runs the cpal stream until the session hands it
/// back, and the last deck takes the stream with it.
#[kithara::test]
fn a_deck_runs_the_cpal_stream_from_attach_to_detach() {
    let mut graph = GraphSession::<CpalStream>::new(start_stream);
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

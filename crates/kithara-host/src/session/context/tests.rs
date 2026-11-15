use std::num::NonZeroU32;

use firewheel::{ActivateInfo, FirewheelConfig, backend::BackendProcessInfo, node::StreamStatus};
use kithara_platform::time::Duration;
use kithara_test_utils::kithara;

use super::{GraphError, SessionGraph};
use crate::rt::MasterNode;

#[kithara::test]
fn pressure_retains_the_real_node_until_two_observed_process_polls() {
    let mut graph = SessionGraph::new(FirewheelConfig {
        channel_capacity: 2,
        ..FirewheelConfig::default()
    });
    let mut processor = graph
        .activate(ActivateInfo {
            sample_rate: NonZeroU32::new(48_000).expect("rate"),
            max_block_frames: NonZeroU32::new(128).expect("block"),
            num_stream_in_channels: 0,
            num_stream_out_channels: 2,
            input_to_output_latency_seconds: 0.0,
        })
        .expect("initial actual output");
    let node = graph.add_node(MasterNode, None).expect("real master node");
    graph
        .connect(node, graph.graph_out_node_id(), &[(0, 0), (1, 1)], true)
        .expect("real output route");
    graph.update().expect("second occupied command slot");
    assert!(matches!(graph.remove_node(node), Err(GraphError::Full)));
    assert!(
        graph.node_info(node).is_some(),
        "pressure precedes graph mutation"
    );
    let block = || BackendProcessInfo {
        frames: 1,
        process_timestamp: None,
        duration_since_stream_start: Duration::ZERO,
        input_stream_status: StreamStatus::empty(),
        output_stream_status: StreamStatus::empty(),
        dropped_frames: 0,
        process_to_playback_delay: None,
    };
    processor.render(&mut [0.0; 2], block());
    assert!(
        matches!(graph.remove_node(node), Err(GraphError::Full)),
        "one publication is not a queue receipt"
    );
    assert!(graph.node_info(node).is_some());
    processor.render(&mut [0.0; 2], block());
    graph
        .remove_node(node)
        .expect("reserved removal after a subsequent poll");
    graph
        .update()
        .expect("complete removal schedule fits its actual reservation");
    assert!(graph.node_info(node).is_none());
    let before = graph.heartbeat();
    drop(processor);
    assert_eq!(
        graph.heartbeat(),
        before,
        "cold settlement adds no audio frames"
    );
    assert!(!graph.is_active());
}

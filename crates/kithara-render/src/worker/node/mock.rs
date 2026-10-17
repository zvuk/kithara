use std::num::NonZeroUsize;

use kithara_audio::TrackStep;
use kithara_command::{ChannelConfig, Sender, channel};
use kithara_effects::EffectDrain;
use kithara_signal::{AudioChunk, AudioChunkInfo};
use kithara_stream::{Activity, ActivityWriter};
use kithara_warp::{Warp, WarpConfig};

use super::{DecoderNode, pending::PendingPacket};
use crate::{
    WarpSource,
    mock::{ScriptedSource, pcm_fixture::PcmFixture},
    test_pools::{TestPools, pools},
    worker::PcmReceiver,
};
pub(in crate::worker::node) struct NodeFixture {
    pub(in crate::worker::node) node: DecoderNode<ScriptedSource, TestPools>,
    pub(in crate::worker::node) receiver: PcmReceiver,
    pub(in crate::worker::node) activity: Activity,
    pub(in crate::worker::node) lane: Sender<crate::LaneProtocol>,
}
impl NodeFixture {
    pub(in crate::worker::node) async fn new(capacity: usize) -> Self {
        Self::with_steps(capacity, []).await
    }

    pub(in crate::worker::node) async fn with_steps(
        capacity: usize,
        steps: impl IntoIterator<Item = TrackStep<AudioChunk>>,
    ) -> Self {
        let mut fixture = PcmFixture::new(capacity, false).await;
        let writer = ActivityWriter::new();
        let activity = writer.reader();
        let spec = fixture.receiver.as_ref().expect("receiver").spec();
        let (lane, inbox) = channel(ChannelConfig::builder().build());
        let pools = pools();
        let source = WarpSource::new(
            ScriptedSource::new(steps),
            Warp::new((), &WarpConfig::builder().build()).renderer(spec, pools.clone()),
            Vec::new(),
            EffectDrain::new(0, &pools).expect("empty effect drain"),
            spec,
            pools.clone(),
            crate::LaneSetup {
                inbox,
                preload_chunks: NonZeroUsize::new(1).expect("preload"),
                declick: crate::consts::DEFAULT_DECLICK,
            },
        );
        Self {
            node: DecoderNode::new(
                source,
                fixture.producer.take().expect("producer owner"),
                Some(writer),
                AudioChunkInfo {
                    spec,
                    ..AudioChunkInfo::default()
                },
                None,
                pools,
                None,
            ),
            receiver: fixture.receiver.take().expect("receiver owner"),
            activity,
            lane,
        }
    }
    pub(in crate::worker::node) fn stage(&mut self, packet: crate::worker::PcmPacket) {
        assert!(self.node.pending.is_none(), "one pending owner");
        self.node.pending = Some(PendingPacket {
            packet,
            source_end: None,
        });
    }
}

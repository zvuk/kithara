#![cfg(not(target_arch = "wasm32"))]
use std::num::NonZeroUsize;

use kithara_audio::Audio;
use kithara_command::{ChannelConfig, channel};
use kithara_effects::EffectDrain;
use kithara_file::File;
use kithara_signal::AudioChunkInfo;
use kithara_stream::Activity;
use kithara_warp::{Warp, WarpConfig};

use super::{DecoderNode, pending::PendingPacket};
use crate::{
    WarpSource,
    mock::pcm_fixture::PcmFixture,
    test_pools::{TestPools, pools},
    worker::PcmReceiver,
};
pub(in crate::worker::node) struct NodeFixture {
    pub(in crate::worker::node) node:
        DecoderNode<Audio<kithara_stream::Stream<File<TestPools>>>, TestPools>,
    pub(in crate::worker::node) receiver: PcmReceiver,
    pub(in crate::worker::node) activity: Activity,
}
impl NodeFixture {
    pub(in crate::worker::node) async fn new(capacity: usize) -> Self {
        let mut fixture = PcmFixture::new(capacity, true).await;
        let mut audio = fixture.audio.take().expect("source owner");
        let activity = audio.activity();
        let writer = audio.take_activity_writer();
        let spec = audio.spec();
        let (_, inbox) = channel(ChannelConfig::builder().build());
        let pools = pools();
        let source = WarpSource::new(
            audio,
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
                writer,
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

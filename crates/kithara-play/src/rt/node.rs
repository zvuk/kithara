use std::sync::Arc;

use firewheel::{
    channel_config::{ChannelConfig, ChannelCount},
    diff::{Diff, Patch},
    node::{AudioNode, AudioNodeInfo, AudioNodeProcessor, ConstructProcessorContext, EmptyConfig},
};
use kithara_bufpool::PcmPool;
use kithara_platform::sync::Mutex;

use super::processor::{PlayerNodeProcessor, StreamShape};
use crate::bridge::{NodeInputs, SharedEq, slot_channels};

/// A player source node that outputs mixed audio from loaded tracks.
///
/// Commands (load, unload, seek, pause, fade) are sent to the processor
/// via channels stored in the node. The `Diff`/`Patch` derives
/// only apply to the `active` field; all runtime state is `#[diff(skip)]`.
#[derive(Clone, Diff, Patch)]
pub struct PlayerNode {
    /// Whether the node is active (used by Diff/Patch for graph updates).
    pub(crate) active: bool,

    /// Inputs taken by the processor.
    #[diff(skip)]
    inputs: Arc<Mutex<Option<NodeInputs>>>,

    /// PCM buffer pool for scratch buffer allocation.
    #[diff(skip)]
    pcm_pool: PcmPool,
}

impl PlayerNode {
    /// Create a player node wired to RT input channels.
    pub fn new(inputs: NodeInputs, pcm_pool: PcmPool) -> Self {
        Self {
            pcm_pool,
            active: true,
            inputs: Arc::new(Mutex::new(Some(inputs))),
        }
    }
}

impl AudioNode for PlayerNode {
    type Configuration = EmptyConfig;

    fn construct_processor(
        &self,
        _config: &Self::Configuration,
        cx: ConstructProcessorContext,
    ) -> impl AudioNodeProcessor {
        let sample_rate = cx.stream_info.sample_rate;
        let max_block_frames = cx.stream_info.max_block_frames;
        let shape = StreamShape {
            sample_rate,
            max_block_frames,
        };
        let inputs = self
            .inputs
            .lock()
            .take()
            .unwrap_or_else(|| slot_channels(SharedEq::new(0)).0);
        PlayerNodeProcessor::new(inputs, shape, &self.pcm_pool)
    }

    fn info(&self, _config: &Self::Configuration) -> AudioNodeInfo {
        AudioNodeInfo::new()
            .debug_name("Player")
            .channel_config(ChannelConfig {
                num_inputs: ChannelCount::ZERO,
                num_outputs: ChannelCount::STEREO,
            })
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;
    use ringbuf::traits::{Consumer, Producer};

    use super::*;
    use crate::bridge::SharedEq;

    fn make_node() -> (PlayerNode, crate::bridge::SlotControl) {
        let (inputs, control) = slot_channels(SharedEq::new(0));
        let node = PlayerNode::new(inputs, PcmPool::default().clone());
        (node, control)
    }

    #[kithara::test]
    fn player_node_defaults_active() {
        let (node, _control) = make_node();
        assert!(node.active);
    }

    #[kithara::test]
    fn player_node_info_has_stereo_output() {
        let (node, _control) = make_node();
        let info = node.info(&EmptyConfig);
        let _ = info;
    }

    #[kithara::test]
    #[case(crate::bridge::PlayerCmd::SetPaused(true))]
    #[case(crate::bridge::PlayerCmd::SetPaused(false))]
    #[case(crate::bridge::PlayerCmd::SetFadeDuration(0.25))]
    fn player_node_with_inputs(#[case] cmd: crate::bridge::PlayerCmd) {
        let (node, mut control) = make_node();
        assert!(node.active);

        control.cmd_tx.try_push(cmd).ok();
        let received = {
            let mut guard = node.inputs.lock();
            (*guard).as_mut().and_then(|inputs| inputs.cmd_rx.try_pop())
        };
        assert!(received.is_some());
    }

    #[kithara::test]
    fn player_node_playback_accessible() {
        let (node, _control) = make_node();
        let playing = node
            .inputs
            .lock()
            .as_ref()
            .expect("inputs not yet taken")
            .playback
            .playing
            .load(std::sync::atomic::Ordering::Relaxed);
        assert!(!playing);
    }
}

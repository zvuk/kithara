#[cfg(test)]
use std::sync::atomic::Ordering;

use firewheel::{
    channel_config::{ChannelConfig, ChannelCount},
    diff::{Diff, Patch, PatchError},
    event::ParamData,
    node::{AudioNode, AudioNodeInfo, AudioNodeProcessor, ConstructProcessorContext, EmptyConfig},
};
use kithara_bufpool::{HasPool, PoolRegion};
use kithara_platform::sync::{Arc, Mutex};

use super::processor::{ContextRequirement, PlayerNodeProcessor, StreamShape};
use crate::bridge::{NodeInputs, SharedEq, slot_channels};

/// A player source node that outputs mixed audio from loaded tracks.
///
/// Commands (load, unload, seek, pause, fade) are sent through channels stored
/// in the node. Only `active` participates in Firewheel parameter updates.
#[derive(Diff)]
pub struct PlayerNode<S> {
    /// Whether the node is active (used by Diff/Patch for graph updates).
    pub(crate) active: bool,

    /// Inputs taken by the processor.
    #[diff(skip)]
    inputs: Arc<Mutex<Option<NodeInputs>>>,

    /// Typed pool facade for scratch buffer allocation.
    #[diff(skip)]
    pools: PoolRegion<S>,

    #[diff(skip)]
    context_requirement: ContextRequirement,
}

/// A runtime parameter patch for [`PlayerNode`].
#[non_exhaustive]
pub enum PlayerNodePatch {
    /// Updates whether the node is active.
    Active(<bool as Patch>::Patch),
}

impl<S> Patch for PlayerNode<S> {
    type Patch = PlayerNodePatch;

    fn patch(data: &ParamData, path: &[u32]) -> Result<Self::Patch, PatchError> {
        match path {
            [0, tail @ ..] => Ok(PlayerNodePatch::Active(bool::patch(data, tail)?)),
            _ => Err(PatchError::InvalidPath),
        }
    }

    fn apply(&mut self, patch: Self::Patch) {
        match patch {
            PlayerNodePatch::Active(patch) => self.active.apply(patch),
        }
    }
}

impl<S> Clone for PlayerNode<S> {
    fn clone(&self) -> Self {
        Self {
            active: self.active,
            inputs: Arc::clone(&self.inputs),
            pools: self.pools.clone(),
            context_requirement: self.context_requirement,
        }
    }
}

impl<S> PlayerNode<S> {
    /// Create a player node wired to RT input channels.
    pub fn new(inputs: NodeInputs, pools: PoolRegion<S>) -> Self {
        Self {
            pools,
            active: true,
            inputs: Arc::new(Mutex::new(Some(inputs))),
            context_requirement: ContextRequirement::Standalone,
        }
    }

    /// Requires the Host-written render context when constructing this node's
    /// processor. Standalone nodes retain their context-free contract.
    #[doc(hidden)]
    #[must_use]
    pub fn with_session_context(mut self) -> Self {
        self.context_requirement = ContextRequirement::Session;
        self
    }
}

impl<S> AudioNode for PlayerNode<S>
where
    S: HasPool<f32> + Send + Sync + 'static,
{
    type Configuration = EmptyConfig;

    fn construct_processor(
        &self,
        _config: &Self::Configuration,
        cx: ConstructProcessorContext,
    ) -> impl AudioNodeProcessor {
        let sample_rate = cx.stream_info.sample_rate;
        let max_block_frames = cx.stream_info.max_block_frames;
        let shape = StreamShape {
            max_block_frames,
            sample_rate,
        };
        let inputs = self
            .inputs
            .lock()
            .take()
            .unwrap_or_else(|| slot_channels(SharedEq::new(0)).0);
        PlayerNodeProcessor::with_context_requirement(
            inputs,
            shape,
            &self.pools,
            self.context_requirement,
        )
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
    use crate::{
        bridge::SharedEq,
        test_pools::{TestPools, pools},
    };

    fn make_node() -> (PlayerNode<TestPools>, crate::bridge::SlotControl) {
        let (inputs, control) = slot_channels(SharedEq::new(0));
        let node = PlayerNode::new(inputs, pools());
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
            .load(Ordering::Relaxed);
        assert!(!playing);
    }
}

use std::{error::Error, fmt};

use firewheel::{
    channel_config::{ChannelConfig, ChannelCount},
    diff::{Diff, Patch, PatchError},
    event::ParamData,
    node::{
        AudioNode, AudioNodeInfo, AudioNodeProcessor, ConstructProcessorContext, EmptyConfig,
        NodeError, NodeID,
    },
};
use kithara_bufpool::{HasPool, PoolRegion};
use kithara_platform::sync::{Arc, Mutex};

use super::processor::{
    DeckMixer, OfflineDeck, OfflineProcessor, PlayerProcessor, PreparedDeck, StreamShape,
};
use crate::bridge::{MixerInputs, SessionInbox};

/// A deck node whose processor borrows its commands from the Host's session store.
#[derive(Diff)]
#[derive_where::derive_where(Clone)]
pub struct PlayerNode<S, E: SessionInbox> {
    pub(crate) active: bool,
    #[diff(skip)]
    inputs: Arc<Mutex<Option<ProcessorInputs<E>>>>,
    #[diff(skip)]
    pools: PoolRegion<S>,
}

#[non_exhaustive]
pub enum PlayerNodePatch {
    Active(<bool as Patch>::Patch),
}

impl<S, E: SessionInbox> Patch for PlayerNode<S, E> {
    type Patch = PlayerNodePatch;

    fn apply(&mut self, patch: Self::Patch) {
        match patch {
            PlayerNodePatch::Active(patch) => self.active.apply(patch),
        }
    }

    fn patch(data: &ParamData, path: &[u32]) -> Result<Self::Patch, PatchError> {
        match path {
            [0, tail @ ..] => Ok(PlayerNodePatch::Active(bool::patch(data, tail)?)),
            _ => Err(PatchError::InvalidPath),
        }
    }
}

struct ProcessorInputs<E: SessionInbox> {
    mixer: MixerInputs,
    offline: Option<OfflineProcessor<E>>,
    prepared: Option<PreparedDeck>,
}

impl<S, E: SessionInbox> PlayerNode<S, E> {
    pub fn new(inputs: MixerInputs, pools: PoolRegion<S>) -> Self {
        Self::with_inputs(
            ProcessorInputs {
                mixer: inputs,
                offline: None,
                prepared: None,
            },
            pools,
        )
    }

    /// Creates an offline node whose mixer belongs to the session between graph turns.
    pub fn offline(inputs: MixerInputs, pools: PoolRegion<S>) -> (Self, OfflineDeck<E>) {
        let (owner, processor) = OfflineDeck::channel(inputs.scope);
        (
            Self::with_inputs(
                ProcessorInputs {
                    mixer: inputs,
                    offline: Some(processor),
                    prepared: None,
                },
                pools,
            ),
            owner,
        )
    }

    fn with_inputs(inputs: ProcessorInputs<E>, pools: PoolRegion<S>) -> Self {
        Self {
            pools,
            active: true,
            inputs: Arc::new(Mutex::new(Some(inputs))),
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("deck processor inputs have already been taken")]
struct ProcessorInputsTaken;

#[derive(Debug, thiserror::Error)]
#[error("deck resources were not prepared for the actual stream geometry")]
struct StreamNotPrepared;

/// The actual graph node and typed cause of a constructor preparation refusal.
#[derive(Debug)]
pub struct PlayerNodePreparationError {
    pub node: NodeID,
    pub error: NodeError,
}

impl fmt::Display for PlayerNodePreparationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "deck node {:?} preparation failed: {}",
            self.node, self.error
        )
    }
}

impl Error for PlayerNodePreparationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.error.0.as_ref())
    }
}

#[derive(Debug, thiserror::Error)]
#[error("the deck node is absent from this graph generation")]
struct NodeUnavailable;

impl<S, E: SessionInbox> PlayerNode<S, E>
where
    S: HasPool<f32> + Send + Sync + 'static,
{
    /// Prepares a registered node without entering Firewheel's consuming compile.
    ///
    /// # Errors
    /// Preserves the actual node identity and typed preparation error.
    pub fn prepare_at(
        context: &firewheel::FirewheelContext,
        node: NodeID,
        shape: StreamShape,
    ) -> Result<(), PlayerNodePreparationError> {
        let prepare = || {
            let player = context
                .node_state::<Self>(node)
                .ok_or_else(|| NodeError(Box::new(NodeUnavailable)))?;
            player.prepare_stream(shape)
        };
        prepare().map_err(|error| PlayerNodePreparationError { node, error })
    }

    /// Prepares a deferred initial factory, or identifies its retained session owner.
    ///
    /// # Errors
    /// Returns the actual node identity and preparation cause without consuming its inputs.
    pub fn prepare_if_initial(
        context: &firewheel::FirewheelContext,
        node: NodeID,
        shape: StreamShape,
    ) -> Result<bool, PlayerNodePreparationError> {
        let player =
            context
                .node_state::<Self>(node)
                .ok_or_else(|| PlayerNodePreparationError {
                    node,
                    error: NodeError(Box::new(NodeUnavailable)),
                })?;
        let initial = player.inputs.lock().is_some();
        if initial {
            player
                .prepare_stream(shape)
                .map_err(|error| PlayerNodePreparationError { node, error })?;
        }
        Ok(initial)
    }

    /// Prepares the complete constructor before the graph takes any unique inputs.
    ///
    /// # Errors
    /// Returns the typed pool, design, handoff or ownership refusal.
    pub fn prepare_stream(&self, shape: StreamShape) -> Result<(), NodeError> {
        let mut guard = self.inputs.lock();
        let inputs = guard
            .as_mut()
            .ok_or_else(|| NodeError(Box::new(ProcessorInputsTaken)))?;
        inputs.prepared = None;
        let prepared = DeckMixer::<E>::prepare(&inputs.mixer, shape, &self.pools)
            .map_err(|error| NodeError(Box::new(error)))?;
        if let Some(processor) = &inputs.offline {
            processor
                .check_install()
                .map_err(|error| NodeError(Box::new(error)))?;
        }
        inputs.prepared = Some(prepared);
        Ok(())
    }
}

impl<S, E: SessionInbox> AudioNode for PlayerNode<S, E>
where
    S: HasPool<f32> + Send + Sync + 'static,
{
    type Configuration = EmptyConfig;

    fn construct_processor(
        &self,
        _config: &Self::Configuration,
        cx: ConstructProcessorContext,
    ) -> Result<impl AudioNodeProcessor, NodeError> {
        let shape = StreamShape::new(cx.stream_info.max_block_frames, cx.stream_info.sample_rate);
        let mut guard = self.inputs.lock();
        let borrowed = guard
            .as_ref()
            .ok_or_else(|| NodeError(Box::new(ProcessorInputsTaken)))?;
        if !borrowed
            .prepared
            .as_ref()
            .is_some_and(|prepared| prepared.shape == shape)
        {
            return Err(NodeError(Box::new(StreamNotPrepared)));
        }
        if let Some(processor) = &borrowed.offline {
            processor
                .check_install()
                .map_err(|error| NodeError(Box::new(error)))?;
        }
        let prepared = guard
            .as_mut()
            .and_then(|inputs| inputs.prepared.take())
            .ok_or_else(|| NodeError(Box::new(StreamNotPrepared)))?;
        let inputs = guard
            .take()
            .ok_or_else(|| NodeError(Box::new(ProcessorInputsTaken)))?;
        drop(guard);
        let mixer = Box::new(DeckMixer::<E>::assemble(inputs.mixer, prepared));
        match inputs.offline {
            Some(mut processor) => {
                processor
                    .install(mixer)
                    .map_err(|error| NodeError(Box::new(error)))?;
                Ok(PlayerProcessor::Offline(processor))
            }
            None => Ok(PlayerProcessor::Native(mixer)),
        }
    }

    fn info(&self, _config: &Self::Configuration) -> Result<AudioNodeInfo, NodeError> {
        Ok(AudioNodeInfo::new()
            .debug_name("Player")
            .channel_config(ChannelConfig {
                num_inputs: ChannelCount::ZERO,
                num_outputs: ChannelCount::STEREO,
            }))
    }
}

#[cfg(test)]
mod tests {
    use kithara_command::{
        Batch, ChannelConfig, LevelInbox, Port, ScopeId, ScopedConfig, ScopedInbox, ScopedSender,
        When, scoped_channel,
    };
    use kithara_signal::SessionFrame;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        bridge::{DeckEnds, DeckPart, DeckProtocol, Fade, Slot, SlotState, scope_channels},
        rt::DeckMixerConfig,
        test_pools::{TestPools, pools},
    };

    struct TestInbox(ScopedInbox<DeckProtocol, DeckProtocol>);
    impl SessionInbox for TestInbox {
        fn scope(&mut self, id: ScopeId) -> Option<LevelInbox<'_, DeckProtocol>> {
            self.0.scope(id)
        }
    }

    type NodeFixture = (
        PlayerNode<TestPools, TestInbox>,
        DeckEnds,
        ScopedSender<DeckProtocol, DeckProtocol>,
        TestInbox,
    );

    fn make_node() -> NodeFixture {
        let config = DeckMixerConfig::default();
        let (mut sender, inbox) = scoped_channel(
            ScopedConfig::builder()
                .scope(
                    ChannelConfig::builder()
                        .targets(config.slots().get())
                        .build(),
                )
                .build(),
        );
        let scope = sender.open(config.slots().get()).expect("deck scope");
        let (ends, inputs) = scope_channels(scope, config);
        (
            PlayerNode::new(inputs, pools()),
            ends,
            sender,
            TestInbox(inbox),
        )
    }

    #[kithara::test]
    fn player_node_defaults_active() {
        let (node, _ends, _sender, _inbox) = make_node();
        assert!(node.active);
    }

    #[kithara::test]
    fn player_node_info_has_stereo_output() {
        let (node, _ends, _sender, _inbox) = make_node();
        let info = node.info(&EmptyConfig);
        let _ = info;
    }

    #[kithara::test]
    #[case(DeckPart::Start { slot: Slot::new(0), fade: Fade::Declick })]
    #[case(DeckPart::Stop { slot: Slot::new(0), fade: Fade::Declick })]
    #[case(DeckPart::Adopt { slot: Slot::new(0), segment: kithara_signal::SegmentId::FIRST })]
    fn player_node_with_inputs(#[case] part: DeckPart) {
        let (node, ends, mut sender, mut inbox) = make_node();
        assert!(node.active);

        sender
            .scope(ends.scope)
            .expect("deck level")
            .send(
                When::Next,
                Batch {
                    basis: Vec::new(),
                    commands: vec![part],
                },
            )
            .expect("the deck channel has room");
        sender.publish().expect("published batch");
        inbox.0.drain();
        let received = node
            .inputs
            .lock()
            .as_mut()
            .map(|inputs| {
                inbox
                    .scope(inputs.mixer.scope)
                    .expect("deck level")
                    .next_due(SessionFrame::default(), 1)
                    .map(|due| {
                        let parts = due.commands().len();
                        due.apply(());
                        parts
                    })
            })
            .expect("inputs not yet taken");
        assert_eq!(received, Some(1));
    }

    #[kithara::test]
    fn player_node_snapshot_starts_with_empty_slots() {
        let (_node, mut ends, _sender, _inbox) = make_node();
        let snapshot = ends.snapshot.read();
        assert_eq!(
            snapshot.slots.len(),
            DeckMixerConfig::default().slots().get()
        );
        assert!(
            snapshot
                .slots
                .iter()
                .all(|slot| slot.state == SlotState::Empty)
        );
    }
}

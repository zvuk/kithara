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
use ringbuf::{
    HeapCons, HeapRb,
    traits::{Consumer, Split},
};

use super::processor::{
    DeckMixer, OfflineDeck, OfflineProcessor, PreparedDeck, ProcessorParts, StreamShape,
};
use crate::bridge::{MixerInputs, SessionInbox};

/// A deck node whose processor borrows its commands from the Host's session store.
#[derive(Diff)]
#[derive_where::derive_where(Clone)]
pub struct PlayerNode<S, E: SessionInbox> {
    pub(crate) active: bool,
    #[diff(skip)]
    inputs: Arc<Mutex<FactoryInputs<E>>>,
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
    processor: OfflineProcessor<E>,
    prepared: Option<PreparedDeck>,
}

struct FactoryInputs<E: SessionInbox> {
    initial: Option<ProcessorInputs<E>>,
    returned: HeapCons<ProcessorParts<E>>,
}

impl<S, E: SessionInbox> PlayerNode<S, E> {
    /// Creates a native node whose mixer returns to its owner when the stream stops.
    pub fn native(inputs: MixerInputs, pools: PoolRegion<S>) -> (Self, OfflineDeck<E>) {
        Self::owned(inputs, pools, true)
    }

    /// Creates an offline node whose mixer belongs to the session between graph turns.
    pub fn offline(inputs: MixerInputs, pools: PoolRegion<S>) -> (Self, OfflineDeck<E>) {
        Self::owned(inputs, pools, false)
    }

    fn owned(inputs: MixerInputs, pools: PoolRegion<S>, native: bool) -> (Self, OfflineDeck<E>) {
        let (owner, mut processor) = OfflineDeck::channel(inputs.scope);
        if native {
            processor.native();
        }
        (
            Self::with_inputs(
                ProcessorInputs {
                    mixer: inputs,
                    processor,
                    prepared: None,
                },
                pools,
            ),
            owner,
        )
    }

    fn with_inputs(inputs: ProcessorInputs<E>, pools: PoolRegion<S>) -> Self {
        let (recycle, returned) = HeapRb::new(1).split();
        let mut inputs = inputs;
        inputs.processor.recycle_to(recycle);
        Self {
            pools,
            active: true,
            inputs: Arc::new(Mutex::new(FactoryInputs {
                initial: Some(inputs),
                returned,
            })),
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
        let initial = player.inputs.lock().initial.is_some();
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
            .initial
            .as_mut()
            .ok_or_else(|| NodeError(Box::new(ProcessorInputsTaken)))?;
        inputs.prepared = None;
        let prepared = DeckMixer::<E>::prepare(&inputs.mixer, shape, &self.pools)
            .map_err(|error| NodeError(Box::new(error)))?;
        inputs
            .processor
            .check_install()
            .map_err(|error| NodeError(Box::new(error)))?;
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
        if guard.initial.is_none() {
            let parts = guard
                .returned
                .try_pop()
                .ok_or_else(|| NodeError(Box::new(ProcessorInputsTaken)))?;
            let (recycle, returned) = HeapRb::new(1).split();
            guard.returned = returned;
            drop(guard);
            return Ok(OfflineProcessor::from_parts(parts, recycle));
        }
        let borrowed = guard
            .initial
            .as_ref()
            .ok_or_else(|| NodeError(Box::new(ProcessorInputsTaken)))?;
        if !borrowed
            .prepared
            .as_ref()
            .is_some_and(|prepared| prepared.shape == shape)
        {
            return Err(NodeError(Box::new(StreamNotPrepared)));
        }
        borrowed
            .processor
            .check_install()
            .map_err(|error| NodeError(Box::new(error)))?;
        let prepared = guard
            .initial
            .as_mut()
            .and_then(|inputs| inputs.prepared.take())
            .ok_or_else(|| NodeError(Box::new(StreamNotPrepared)))?;
        let inputs = guard
            .initial
            .take()
            .ok_or_else(|| NodeError(Box::new(ProcessorInputsTaken)))?;
        drop(guard);
        let mixer = Box::new(DeckMixer::<E>::assemble(inputs.mixer, prepared));
        let mut processor = inputs.processor;
        processor.install(mixer);
        Ok(processor)
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
    use firewheel::FirewheelConfig;
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
        OfflineDeck<TestInbox>,
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
        let (node, owner) = PlayerNode::native(inputs, pools());
        node.prepare_stream(StreamShape::new(
            std::num::NonZeroU32::new(128).expect("block"),
            std::num::NonZeroU32::new(44_100).expect("rate"),
        ))
        .expect("fixture constructor preparation");
        (node, ends, sender, TestInbox(inbox), owner)
    }

    fn cold_stream(rate: u32) -> firewheel::ActivateInfo {
        use std::num::NonZeroU32;

        firewheel::ActivateInfo {
            sample_rate: NonZeroU32::new(rate).expect("positive test rate"),
            max_block_frames: NonZeroU32::new(128).expect("fixed block"),
            num_stream_in_channels: 0,
            num_stream_out_channels: 2,
            input_to_output_latency_seconds: 0.0,
        }
    }

    fn stop(
        context: &firewheel::FirewheelContext,
        mut processor: firewheel::processor::FirewheelProcessor,
    ) {
        use audioadapter_buffers::direct::InterleavedSlice;
        use firewheel::{backend::BackendProcessInfo, node::StreamStatus};
        processor.process(
            &InterleavedSlice::new(&[] as &[f32], 0, 0).expect("empty inputs"),
            &mut InterleavedSlice::new_mut(&mut [], 2, 0).expect("empty output"),
            BackendProcessInfo {
                frames: 0,
                process_timestamp: context.audio_clock_instant(),
                duration_since_stream_start: kithara_platform::time::Duration::ZERO,
                input_stream_status: StreamStatus::empty(),
                output_stream_status: StreamStatus::empty(),
                dropped_frames: 0,
                process_to_playback_delay: None,
            },
        );
        drop(processor);
    }

    #[kithara::test]
    fn player_node_defaults_active() {
        let (node, _ends, _sender, _inbox, _owner) = make_node();
        assert!(node.active);
    }

    #[kithara::test]
    fn player_node_info_has_stereo_output() {
        let (node, _ends, _sender, _inbox, _owner) = make_node();
        let info = node.info(&EmptyConfig);
        let _ = info;
    }

    #[kithara::test]
    #[case(DeckPart::Start { slot: Slot::new(0), fade: Fade::Declick })]
    #[case(DeckPart::Stop { slot: Slot::new(0), fade: Fade::Declick })]
    #[case(DeckPart::Adopt { slot: Slot::new(0), segment: kithara_signal::SegmentId::FIRST })]
    fn player_node_with_inputs(#[case] part: DeckPart) {
        let (node, ends, mut sender, mut inbox, _owner) = make_node();
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
            .initial
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
        let (_node, mut ends, _sender, _inbox, _owner) = make_node();
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

    #[kithara::test]
    #[case::pool(false)]
    #[case::filter(true)]
    fn cold_refusal_retains_inputs_and_scope_snapshot(#[case] invalid_design: bool) {
        use firewheel::FirewheelContext;
        use kithara_bufpool::PoolError;
        use kithara_dsp::filter::FilterError;
        use kithara_effects::eq::EqError;

        const BUDGET: usize = 1024 * 1024;
        let (mut node, mut ends, _sender, _inbox, _owner) = make_node();
        let initial = ends.snapshot.read().clone();
        let pointer =
            std::ptr::from_ref(node.inputs.lock().initial.as_ref().expect("owned inputs"));
        node.pools = crate::test_pools::pools_with_budget(BUDGET);
        let held = (!invalid_design).then(|| {
            node.pools
                .get_with_len::<f32>(BUDGET / 4)
                .expect("controlled pool pressure")
        });
        let mut context = FirewheelContext::new(FirewheelConfig::default());
        let id = context
            .add_node(node.clone(), None)
            .expect("cold deck node");
        let refused = cold_stream(if invalid_design { 12_000 } else { 44_100 });
        let PlayerNodePreparationError {
            node: failed,
            error,
        } = PlayerNode::<TestPools, TestInbox>::prepare_at(
            &context,
            id,
            StreamShape::new(refused.max_block_frames, refused.sample_rate),
        )
        .expect_err("controlled cold refusal");
        assert_eq!(failed, id);
        let error = error
            .0
            .downcast_ref::<EqError>()
            .expect("canonical preparation error");
        assert_eq!(
            matches!(error, EqError::Filter(FilterError::Parameters)),
            invalid_design
        );
        assert_eq!(
            matches!(
                error,
                EqError::Pool(PoolError::OverallBudgetExceeded {
                    max_bytes: BUDGET,
                    ..
                })
            ),
            !invalid_design
        );
        assert_eq!(
            std::ptr::from_ref(
                node.inputs
                    .lock()
                    .initial
                    .as_ref()
                    .expect("same owned inputs")
            ),
            pointer
        );
        assert_eq!(*ends.snapshot.read(), initial);
        drop(held);
        let admitted = cold_stream(48_000);
        PlayerNode::<TestPools, TestInbox>::prepare_at(
            &context,
            id,
            StreamShape::new(admitted.max_block_frames, admitted.sample_rate),
        )
        .expect("valid owner preparation");
        let processor = context
            .activate(admitted)
            .expect("valid retry uses the same inputs");
        assert!(node.inputs.lock().initial.is_none());
        assert_eq!(*ends.snapshot.read(), initial);
        stop(&context, processor);
        assert_eq!(
            *ends.snapshot.read(),
            initial,
            "no synthetic pre-render publication"
        );
    }

    #[kithara::test]
    fn later_factory_refusal_and_unrendered_stream_preserve_scope_snapshot() {
        use std::num::NonZeroU32;

        use audioadapter_buffers::direct::InterleavedSlice;
        use firewheel::{FirewheelContext, backend::BackendProcessInfo, node::StreamStatus};
        use kithara_platform::time::Duration;
        use kithara_signal::{OutputContext, SessionEpoch};
        use kithara_warp::RenderContext;
        type Refusal = kithara_effects::eq::EqError;

        let (node, mut ends, _sender, mut inbox, mut owner) = make_node();
        let initial = ends.snapshot.read().clone();
        let mut context = FirewheelContext::new(FirewheelConfig::default());
        let store = context.proc_store_mut().expect("cold processor store");
        inbox.0.drain();
        assert!(store.insert(inbox).is_ok(), "session inbox slot is vacant");
        crate::rt::install_render_context(store).expect("render context slot");
        let running = context
            .activate(cold_stream(44_100))
            .expect("actual empty output");
        let id = context.add_node(node.clone(), None).expect("deck node");
        context
            .connect(id, context.graph_out_node_id(), &[(0, 0), (1, 1)], true)
            .expect("stereo graph route");
        context
            .update()
            .expect("first actual factory succeeds before later refusal");
        let (mut later_node, _later_ends, _later_sender, _later_inbox, mut later_owner) =
            make_node();
        const BUDGET: usize = 1024 * 1024;
        later_node.pools = crate::test_pools::pools_with_budget(BUDGET);
        let held = later_node
            .pools
            .get_with_len::<f32>(BUDGET / 4)
            .expect("controlled later pool pressure");
        let later = context
            .add_node(later_node.clone(), None)
            .expect("later real deck");
        let attempted = cold_stream(44_100);
        let PlayerNodePreparationError {
            node: failed,
            error,
        } = PlayerNode::<TestPools, TestInbox>::prepare_at(
            &context,
            later,
            StreamShape::new(attempted.max_block_frames, attempted.sample_rate),
        )
        .expect_err("later owner refuses before consuming compile");
        assert_eq!(failed, later);
        assert!(error.0.is::<Refusal>());
        assert!(matches!(
            error.0.downcast_ref::<Refusal>(),
            Some(Refusal::Pool(_))
        ));
        assert!(
            node.inputs.lock().initial.is_none(),
            "the first factory succeeded"
        );
        assert_eq!(*ends.snapshot.read(), initial);
        drop(held);
        stop(&context, running);
        let admitted = cold_stream(48_000);
        owner.reserve_stream().expect("returned first owner");
        owner
            .prepare_stream(StreamShape::new(
                admitted.max_block_frames,
                admitted.sample_rate,
            ))
            .expect("first owner's actual rate");
        PlayerNode::<TestPools, TestInbox>::prepare_at(
            &context,
            later,
            StreamShape::new(admitted.max_block_frames, admitted.sample_rate),
        )
        .expect("later owner prepares");
        let processor = context
            .activate(admitted)
            .expect("retained deck prepares actual rate");
        owner.resume_stream().expect("reserved first handoff");
        assert_eq!(*ends.snapshot.read(), initial);
        stop(&context, processor);
        assert_eq!(
            *ends.snapshot.read(),
            initial,
            "dropping before callback publishes nothing"
        );

        let rate = NonZeroU32::new(48_000).expect("actual rate");
        crate::rt::publish_render_context(
            context.proc_store_mut().expect("returned processor store"),
            RenderContext::new_linear(
                OutputContext::new(
                    SessionFrame::new(0)..SessionFrame::new(128),
                    rate,
                    SessionEpoch::new(0),
                    None,
                )
                .expect("first output block"),
                None,
            )
            .expect("linear render context"),
        )
        .expect("published callback context");
        for owner in [&mut owner, &mut later_owner] {
            owner.reserve_stream().expect("cold reservation");
            owner
                .prepare_stream(StreamShape::new(admitted.max_block_frames, rate))
                .expect("matching preparation");
        }
        let mut processor = context
            .activate(cold_stream(rate.get()))
            .expect("same geometry retries prepare anew");
        for owner in [&mut owner, &mut later_owner] {
            owner.resume_stream().expect("reserved native handoff");
        }
        let input = InterleavedSlice::new(&[] as &[f32], 0, 0).expect("empty inputs");
        let mut samples = [f32::NAN; 256];
        processor.process(
            &input,
            &mut InterleavedSlice::new_mut(&mut samples, 2, 128).expect("stereo outputs"),
            BackendProcessInfo {
                frames: 128,
                process_timestamp: None,
                duration_since_stream_start: Duration::ZERO,
                input_stream_status: StreamStatus::empty(),
                output_stream_status: StreamStatus::empty(),
                dropped_frames: 0,
                process_to_playback_delay: None,
            },
        );
        assert_eq!(samples, [0.0; 256]);
        let snapshot = ends.snapshot.read();
        assert_eq!(snapshot.sample_rate, rate.get());
        assert_eq!(snapshot.blocks, 1);
        assert_eq!(snapshot.slots, initial.slots);
        assert_eq!(snapshot.eq, initial.eq);
        assert_eq!(snapshot.metrics, initial.metrics);
    }
    #[kithara::test]
    #[case::queued(false)]
    #[case::installed(true)]
    fn a_retired_eq_cannot_refuse_the_replacement_stream(#[case] installed: bool) {
        use std::num::NonZeroU32;

        use audioadapter_buffers::direct::InterleavedSlice;
        use firewheel::{FirewheelContext, backend::BackendProcessInfo, node::StreamStatus};
        use kithara_platform::time::Duration;
        use kithara_signal::{OutputContext, SessionEpoch};
        use kithara_warp::RenderContext;

        let (node, mut ends, _sender, inbox, _owner) = make_node();
        let mut context = FirewheelContext::new(FirewheelConfig::default());
        let id = context.add_node(node, None).expect("deck node");
        context
            .connect(id, context.graph_out_node_id(), &[(0, 0), (1, 1)], true)
            .expect("stereo route");
        let store = context.proc_store_mut().expect("cold store");
        assert!(store.insert(inbox).is_ok());
        crate::rt::install_render_context(store).expect("context slot");
        let rate = NonZeroU32::new(44_100).expect("initial rate");
        crate::rt::publish_render_context(
            store,
            RenderContext::new_linear(
                OutputContext::new(
                    SessionFrame::new(0)..SessionFrame::new(128),
                    rate,
                    SessionEpoch::new(0),
                    None,
                )
                .expect("output range"),
                None,
            )
            .expect("linear context"),
        )
        .expect("publish context");
        let mut processor = context
            .activate(cold_stream(44_100))
            .expect("initial stream");
        if installed {
            let input = InterleavedSlice::new(&[] as &[f32], 0, 0).expect("no inputs");
            let mut samples = [f32::NAN; 256];
            processor.process(
                &input,
                &mut InterleavedSlice::new_mut(&mut samples, 2, 128).expect("stereo output"),
                BackendProcessInfo {
                    frames: 128,
                    process_timestamp: None,
                    duration_since_stream_start: Duration::ZERO,
                    input_stream_status: StreamStatus::empty(),
                    output_stream_status: StreamStatus::empty(),
                    dropped_frames: 0,
                    process_to_playback_delay: None,
                },
            );
            assert_eq!(samples, [0.0; 256]);
        }
        stop(&context, processor);
        let before = ends.snapshot.read().clone();
        context.remove_node(id).expect("retire exact generation");
        let processor = context
            .activate(cold_stream(12_000))
            .expect("a retired EQ is not a participant in new stream admission");
        assert_eq!(*ends.snapshot.read(), before);
        stop(&context, processor);
    }
}

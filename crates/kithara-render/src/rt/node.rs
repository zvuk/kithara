use std::marker::PhantomData;

use firewheel::{
    channel_config::{ChannelConfig, ChannelCount},
    diff::{Diff, Patch, PatchError},
    event::ParamData,
    node::{
        AudioNode, AudioNodeInfo, AudioNodeProcessor, ConstructProcessorContext, EmptyConfig,
        NodeError,
    },
};
use kithara_bufpool::{HasPool, PoolRegion};
use kithara_platform::sync::{Arc, Mutex};

use super::processor::{DeckMixer, StreamShape};
use crate::bridge::{MixerInputs, SessionInbox};

/// A deck node whose processor borrows its commands from the Host's session store.
#[derive(Diff)]
#[derive_where::derive_where(Clone)]
pub struct PlayerNode<S, E: SessionInbox> {
    pub(crate) active: bool,
    #[diff(skip)]
    inputs: Arc<Mutex<Option<MixerInputs>>>,
    #[diff(skip)]
    pools: PoolRegion<S>,
    #[diff(skip)]
    session: PhantomData<fn() -> E>,
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

impl<S, E: SessionInbox> PlayerNode<S, E> {
    pub fn new(inputs: MixerInputs, pools: PoolRegion<S>) -> Self {
        Self {
            pools,
            active: true,
            inputs: Arc::new(Mutex::new(Some(inputs))),
            session: PhantomData,
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("deck processor inputs have already been taken")]
struct ProcessorInputsTaken;

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
        let mut inputs = self.inputs.lock();
        let borrowed = inputs.as_ref()
            .ok_or_else(|| NodeError(Box::new(ProcessorInputsTaken)))?;
        let prepared = DeckMixer::<E>::prepare(borrowed, shape, &self.pools)
            .map_err(|error| NodeError(Box::new(error)))?;
        let inputs = inputs.take().ok_or_else(|| NodeError(Box::new(ProcessorInputsTaken)))?;
        Ok(DeckMixer::<E>::assemble(inputs, prepared))
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
                    .scope(inputs.scope)
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

    #[kithara::test]
    #[case::pool(false)]
    #[case::filter(true)]
    fn cold_refusal_retains_inputs_and_scope_snapshot(#[case] invalid_design: bool) {
        use firewheel::{FirewheelContext, error::{ActivateError, CompileGraphError}};
        use kithara_bufpool::PoolError;
        use kithara_dsp::filter::FilterError;
        use kithara_effects::EffectPrepareError;

        const BUDGET: usize = 1024 * 1024;
        let (mut node, mut ends, _sender, _inbox) = make_node();
        let initial = ends.snapshot.read().clone();
        let pointer = std::ptr::from_ref(node.inputs.lock().as_ref().expect("owned inputs"));
        node.pools = crate::test_pools::pools_with_budget(BUDGET);
        let held = (!invalid_design).then(|| node.pools.get_with_len::<f32>(BUDGET / 4).expect("controlled pool pressure"));
        let mut context = FirewheelContext::new(Default::default());
        let id = context.add_node(node.clone(), None).expect("cold deck node");
        let refused = cold_stream(if invalid_design { 12_000 } else { 44_100 });
        let error = context.activate(refused).err().expect("controlled cold refusal");
        let ActivateError::GraphCompileError(CompileGraphError::ProcessorConstructionFailed { node: failed, error }) = error else {
            panic!("the cold factory preserves its typed error");
        };
        assert_eq!(failed, id);
        let error = error.0.downcast_ref::<EffectPrepareError>().expect("canonical preparation error");
        assert_eq!(matches!(error, EffectPrepareError::Filter(FilterError::Parameters)), invalid_design);
        assert_eq!(matches!(error, EffectPrepareError::Pool(PoolError::OverallBudgetExceeded { max_bytes: BUDGET, .. })), !invalid_design);
        assert_eq!(std::ptr::from_ref(node.inputs.lock().as_ref().expect("same owned inputs")), pointer);
        assert_eq!(*ends.snapshot.read(), initial);
        drop(held);
        let admitted = cold_stream(48_000);
        let processor = context.activate(admitted).expect("valid retry uses the same inputs");
        assert!(node.inputs.lock().is_none());
        assert_eq!(*ends.snapshot.read(), initial);
        drop(processor);
        assert_eq!(*ends.snapshot.read(), initial, "no synthetic pre-render publication");
    }

    #[kithara::test]
    fn later_factory_refusal_and_unrendered_stream_preserve_scope_snapshot() {
        use std::{num::NonZeroU32, sync::atomic::{AtomicBool, Ordering}};

        use audioadapter_buffers::direct::InterleavedSlice;
        use firewheel::{
            FirewheelContext, backend::BackendProcessInfo,
            error::{ActivateError, CompileGraphError},
            node::StreamStatus,
        };
        use kithara_platform::time::Duration;
        use kithara_signal::{OutputContext, SessionEpoch};
        use kithara_warp::RenderContext;

        #[derive(Debug, thiserror::Error)]
        #[error("controlled later factory refusal")]
        struct Refusal;

        struct LaterNode(Arc<AtomicBool>);
        struct LaterProcessor;
        impl AudioNodeProcessor for LaterProcessor {}
        impl AudioNode for LaterNode {
            type Configuration = EmptyConfig;

            fn info(&self, _: &EmptyConfig) -> Result<AudioNodeInfo, NodeError> {
                Ok(AudioNodeInfo::new())
            }

            fn construct_processor(
                &self, _: &EmptyConfig, _: ConstructProcessorContext,
            ) -> Result<impl AudioNodeProcessor, NodeError> {
                if self.0.load(Ordering::SeqCst) {
                    Err(NodeError(Box::new(Refusal)))
                } else {
                    Ok(LaterProcessor)
                }
            }
        }

        let (node, mut ends, _sender, mut inbox) = make_node();
        let initial = ends.snapshot.read().clone();
        let refused = Arc::new(AtomicBool::new(true));
        let mut context = FirewheelContext::new(Default::default());
        let id = context.add_node(node.clone(), None).expect("deck node");
        context.connect(id, context.graph_out_node_id(), &[(0, 0), (1, 1)], true)
            .expect("stereo graph route");
        let later = context.add_node(LaterNode(refused.clone()), None).expect("later factory");
        let store = context.proc_store_mut().expect("cold processor store");
        inbox.0.drain();
        assert!(store.insert(inbox).is_ok(), "session inbox slot is vacant");
        crate::rt::install_render_context(store).expect("render context slot");
        let error = context.activate(cold_stream(44_100)).err().expect("later factory refuses");
        let ActivateError::GraphCompileError(CompileGraphError::ProcessorConstructionFailed { node: failed, error }) = error else {
            panic!("initial factory error preserves its typed payload and node");
        };
        assert_eq!(failed, later);
        assert!(error.0.is::<Refusal>());
        assert!(node.inputs.lock().is_none(), "the first factory succeeded");
        assert_eq!(*ends.snapshot.read(), initial);

        refused.store(false, Ordering::SeqCst);
        let processor = context.activate(cold_stream(48_000)).expect("retained deck prepares actual rate");
        assert_eq!(*ends.snapshot.read(), initial);
        drop(processor);
        assert_eq!(*ends.snapshot.read(), initial, "dropping before callback publishes nothing");

        let rate = NonZeroU32::new(48_000).expect("actual rate");
        crate::rt::publish_render_context(
            context.proc_store_mut().expect("returned processor store"),
            RenderContext::new_linear(
                OutputContext::new(SessionFrame::new(0)..SessionFrame::new(128),
                    rate, SessionEpoch::new(0), None).expect("first output block"),
                None,
            ).expect("linear render context"),
        ).expect("published callback context");
        let mut processor = context.activate(cold_stream(rate.get())).expect("same geometry retries prepare anew");
        let input = InterleavedSlice::new(&[] as &[f32], 0, 0).expect("empty inputs");
        let mut samples = [f32::NAN; 256];
        processor.process(&input,
            &mut InterleavedSlice::new_mut(&mut samples, 2, 128).expect("stereo outputs"),
            BackendProcessInfo {
                frames: 128,
                process_timestamp: None,
                duration_since_stream_start: Duration::ZERO,
                input_stream_status: StreamStatus::empty(),
                output_stream_status: StreamStatus::empty(),
                dropped_frames: 0,
                process_to_playback_delay: None,
            });
        assert_eq!(samples, [0.0; 256]);
        let snapshot = ends.snapshot.read();
        assert_eq!(snapshot.sample_rate, rate.get());
        assert_eq!(snapshot.blocks, 1);
        assert_eq!(snapshot.slots, initial.slots);
        assert_eq!(snapshot.eq, initial.eq);
        assert_eq!(snapshot.metrics, initial.metrics);
    }
}

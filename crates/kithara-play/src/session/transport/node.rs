use firewheel::{
    FirewheelCtx,
    backend::AudioBackend,
    clock::{EventInstant, InstantSamples},
    event::{NodeEventType, ProcEvents},
    node::{
        AudioNode, AudioNodeInfo, AudioNodeProcessor, ConstructProcessorContext, EmptyConfig,
        NodeID, ProcBuffers, ProcExtra, ProcInfo, ProcStreamCtx, ProcessStatus,
    },
};
use kithara_audio::SessionAnchorCell;
use kithara_platform::sync::Arc;
use triple_buffer::{Output, triple_buffer};

use super::{
    commit::{TransportCommitEvent, TransportCommitStamp, TransportObservation},
    process::{
        TransportCommitState, TransportObservationInput, process_transport, restart_transport,
    },
};
use crate::api::TransportRevision;

pub(crate) fn install<B: AudioBackend>(
    ctx: &mut FirewheelCtx<B>,
) -> Result<TransportControl, &'static str> {
    let (observation_input, observation_output) = triple_buffer(&TransportObservation::default());
    let anchor = SessionAnchorCell::new();
    let store = ctx
        .proc_store_mut()
        .ok_or("session transport store is unavailable while the stream is running")?;
    store
        .insert(TransportCommitState::new(Arc::clone(&anchor)))
        .map_err(|_| "session transport state store slot already exists")?;
    store
        .insert(TransportObservationInput::new(observation_input))
        .map_err(|_| "session transport observation store slot already exists")?;
    let node_id = ctx.add_node(SessionTransportNode, None);
    Ok(TransportControl::new(node_id, observation_output, anchor))
}

#[derive(Debug)]
pub(crate) struct TransportControl {
    /// The grid this transport publishes; handed to every deck that binds.
    anchor: Arc<SessionAnchorCell>,
    node_id: NodeID,
    observation: Output<TransportObservation>,
}

impl TransportControl {
    const fn new(
        node_id: NodeID,
        observation: Output<TransportObservation>,
        anchor: Arc<SessionAnchorCell>,
    ) -> Self {
        Self {
            anchor,
            node_id,
            observation,
        }
    }

    pub(crate) fn anchor(&self) -> Arc<SessionAnchorCell> {
        Arc::clone(&self.anchor)
    }

    delegate::delegate! {
        to self.observation {
            #[expr(*$)]
            #[call(read)]
            pub(crate) fn observation(&mut self) -> TransportObservation;
        }
    }

    pub(crate) fn queue_abort<B: AudioBackend>(
        &self,
        ctx: &mut FirewheelCtx<B>,
        revision: TransportRevision,
    ) {
        ctx.queue_event_for(
            self.node_id,
            NodeEventType::custom(TransportCommitEvent::Abort(revision)),
        );
    }

    pub(crate) fn queue_stamp<B: AudioBackend>(
        &self,
        ctx: &mut FirewheelCtx<B>,
        stamp: TransportCommitStamp,
    ) {
        ctx.queue_event_for(
            self.node_id,
            NodeEventType::custom(TransportCommitEvent::Stage(stamp)),
        );
        ctx.schedule_event_for(
            self.node_id,
            NodeEventType::custom(TransportCommitEvent::Apply(stamp.revision())),
            Some(EventInstant::Samples(InstantSamples(i64::from(
                stamp.target_frame(),
            )))),
        );
    }
}

pub(crate) struct SessionTransportNode;

impl AudioNode for SessionTransportNode {
    type Configuration = EmptyConfig;

    fn info(&self, _configuration: &Self::Configuration) -> AudioNodeInfo {
        AudioNodeInfo::new()
            .debug_name("SessionTransport")
            .is_pre_process()
    }

    fn construct_processor(
        &self,
        _configuration: &Self::Configuration,
        _cx: ConstructProcessorContext,
    ) -> impl AudioNodeProcessor {
        SessionTransportProcessor
    }
}

pub(crate) struct SessionTransportProcessor;

impl AudioNodeProcessor for SessionTransportProcessor {
    fn stream_stopped(&mut self, context: &mut ProcStreamCtx) {
        if let Err(error) = restart_transport(context.store) {
            let _ = context.logger.try_error(error.message());
        }
    }

    #[kithara_test_utils::kithara::rtsan_forbid_blocking]
    fn process(
        &mut self,
        info: &ProcInfo,
        _buffers: ProcBuffers,
        events: &mut ProcEvents,
        extra: &mut ProcExtra,
    ) -> ProcessStatus {
        if let Err(error) = process_transport(info, events, &mut extra.store) {
            let _ = extra.logger.try_error(error.message());
        }
        ProcessStatus::ClearAllOutputs
    }
}

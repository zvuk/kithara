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
use kithara_play::rt::{install_render_context, invalidate_render_context, publish_render_context};
use kithara_test_utils::kithara::rtsan_forbid_blocking;
use kithara_warp::{RenderContext, SessionFrame};
use triple_buffer::{Output, triple_buffer};

use super::{
    commit::{
        SessionGridGeneration, TransportCommitEvent, TransportCommitStamp, TransportObservation,
    },
    process::{
        TransportCommitState, TransportFrame, TransportObservationInput, process_transport,
        restart_transport,
    },
};
use crate::api::TransportRevision;

pub(crate) fn install<B: AudioBackend>(
    ctx: &mut FirewheelCtx<B>,
    session_grid: SessionGridGeneration,
) -> Result<TransportControl, &'static str> {
    let initial = TransportObservation::new(None, None, session_grid);
    let (observation_input, observation_output) = triple_buffer(&initial);
    let store = ctx
        .proc_store_mut()
        .ok_or("session transport store is unavailable while the stream is running")?;
    install_render_context(store)?;
    store
        .insert(TransportCommitState::new(session_grid))
        .map_err(|_| "session transport state store slot already exists")?;
    store
        .insert(TransportObservationInput::new(observation_input))
        .map_err(|_| "session transport observation store slot already exists")?;
    let node_id = ctx.add_node(SessionTransportNode, None);
    Ok(TransportControl::new(node_id, observation_output))
}

#[derive(Debug)]
pub(crate) struct TransportControl {
    node_id: NodeID,
    observation: Output<TransportObservation>,
}

impl TransportControl {
    const fn new(node_id: NodeID, observation: Output<TransportObservation>) -> Self {
        Self {
            node_id,
            observation,
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

    delegate::delegate! {
        to self.observation {
            #[expr(*$)]
            #[call(read)]
            pub(crate) fn observation(&mut self) -> TransportObservation;
        }
    }
}

pub(crate) struct SessionTransportNode;

impl AudioNode for SessionTransportNode {
    type Configuration = EmptyConfig;

    fn construct_processor(
        &self,
        _configuration: &Self::Configuration,
        _cx: ConstructProcessorContext,
    ) -> impl AudioNodeProcessor {
        SessionTransportProcessor
    }

    fn info(&self, _configuration: &Self::Configuration) -> AudioNodeInfo {
        AudioNodeInfo::new()
            .debug_name("SessionTransport")
            .is_pre_process()
    }
}

pub(crate) struct SessionTransportProcessor;

impl AudioNodeProcessor for SessionTransportProcessor {
    fn stream_stopped(&mut self, context: &mut ProcStreamCtx) {
        if invalidate_render_context(context.store).is_err() {
            let _ = context
                .logger
                .try_error("render context store slot is missing");
        }
        if let Err(error) = restart_transport(context.store) {
            let _ = context.logger.try_error(error.message());
        }
    }

    #[rtsan_forbid_blocking]
    fn process(
        &mut self,
        info: &ProcInfo,
        _buffers: ProcBuffers,
        events: &mut ProcEvents,
        extra: &mut ProcExtra,
    ) -> ProcessStatus {
        let processed = process_transport(info, events, &mut extra.store);
        let context = processed
            .as_ref()
            .ok()
            .and_then(|transport| build(info, transport));
        let invalid = context.is_none();
        let replaced = match context {
            Some(context) => publish_render_context(&mut extra.store, context),
            None => invalidate_render_context(&mut extra.store),
        };
        if let Err(error) = replaced {
            let _ = extra.logger.try_error(error);
        } else if invalid {
            let message = processed
                .err()
                .map_or("render context is invalid", |error| error.message());
            let _ = extra.logger.try_error(message);
        }
        ProcessStatus::ClearAllOutputs
    }
}

fn build(info: &ProcInfo, transport: &TransportFrame) -> Option<RenderContext> {
    let output_frames = info.clock_samples_range();
    RenderContext::new(
        SessionFrame::new(output_frames.start.0)..SessionFrame::new(output_frames.end.0),
        info.sample_rate,
        transport.session_beats.clone(),
        transport.session_epoch,
        transport.transport_revision,
    )
}

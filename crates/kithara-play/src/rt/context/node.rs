use firewheel::{
    FirewheelCtx,
    backend::AudioBackend,
    event::ProcEvents,
    node::{
        AudioNode, AudioNodeInfo, AudioNodeProcessor, ConstructProcessorContext, EmptyConfig,
        ProcBuffers, ProcExtra, ProcInfo, ProcStreamCtx, ProcessStatus,
    },
};
use triple_buffer::triple_buffer;

use super::commit::{
    RenderContextControl, TransportCommitState, TransportObservation, TransportObservationInput,
    process_transport, restart_transport,
};

pub(crate) fn install<B: AudioBackend>(
    ctx: &mut FirewheelCtx<B>,
) -> Result<RenderContextControl, &'static str> {
    let (observation_input, observation_output) = triple_buffer(&TransportObservation::default());
    let store = ctx
        .proc_store_mut()
        .ok_or("render context store is unavailable while the stream is running")?;
    store
        .insert(TransportCommitState::default())
        .map_err(|_| "transport commit state store slot already exists")?;
    store
        .insert(TransportObservationInput::new(observation_input))
        .map_err(|_| "transport observation store slot already exists")?;
    let node_id = ctx.add_node(RenderContextNode, None);
    Ok(RenderContextControl::new(node_id, observation_output))
}

pub(super) struct RenderContextNode;

impl AudioNode for RenderContextNode {
    type Configuration = EmptyConfig;

    fn info(&self, _configuration: &Self::Configuration) -> AudioNodeInfo {
        AudioNodeInfo::new()
            .debug_name("RenderContext")
            .is_pre_process()
    }

    fn construct_processor(
        &self,
        _configuration: &Self::Configuration,
        _cx: ConstructProcessorContext,
    ) -> impl AudioNodeProcessor {
        RenderContextProcessor
    }
}

pub(super) struct RenderContextProcessor;

impl AudioNodeProcessor for RenderContextProcessor {
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

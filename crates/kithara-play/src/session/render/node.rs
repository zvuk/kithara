use firewheel::{
    FirewheelCtx,
    backend::AudioBackend,
    event::ProcEvents,
    node::{
        AudioNode, AudioNodeInfo, AudioNodeProcessor, ConstructProcessorContext, EmptyConfig,
        ProcBuffers, ProcExtra, ProcInfo, ProcStore, ProcStreamCtx, ProcessStatus,
    },
};
use triple_buffer::triple_buffer;

use super::{
    RenderContext, RenderFrame,
    commit::{
        RenderContextControl, TransportCommitState, TransportFrame, TransportObservation,
        TransportObservationInput, process_transport, restart_transport,
    },
};

#[derive(Debug, Default)]
pub(super) enum RenderContextSlot {
    #[default]
    Unwritten,
    Ready(RenderContext),
    Invalid,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RenderContextUnavailable {
    Missing,
    Unwritten,
    Invalid,
    Stale,
}

impl RenderContextUnavailable {
    pub(crate) const fn message(self) -> &'static str {
        match self {
            Self::Missing => "render context store slot is missing",
            Self::Unwritten => "render context was not written before player processing",
            Self::Invalid => "render context is invalid",
            Self::Stale => "render context does not match the player process block",
        }
    }
}

pub(crate) fn install<B: AudioBackend>(
    ctx: &mut FirewheelCtx<B>,
) -> Result<RenderContextControl, &'static str> {
    let (observation_input, observation_output) = triple_buffer(&TransportObservation::default());
    let store = ctx
        .proc_store_mut()
        .ok_or("render context store is unavailable while the stream is running")?;
    store
        .insert(RenderContextSlot::default())
        .map_err(|_| "render context store slot already exists")?;
    store
        .insert(TransportCommitState::default())
        .map_err(|_| "transport commit state store slot already exists")?;
    store
        .insert(TransportObservationInput::new(observation_input))
        .map_err(|_| "transport observation store slot already exists")?;
    let node_id = ctx.add_node(RenderContextNode, None);
    Ok(RenderContextControl::new(node_id, observation_output))
}

pub(crate) fn read<'a>(
    store: &'a ProcStore,
    info: &ProcInfo,
) -> Result<&'a RenderContext, RenderContextUnavailable> {
    let slot = store
        .try_get::<RenderContextSlot>()
        .ok_or(RenderContextUnavailable::Missing)?;
    let context = match slot {
        RenderContextSlot::Unwritten => return Err(RenderContextUnavailable::Unwritten),
        RenderContextSlot::Ready(context) => context,
        RenderContextSlot::Invalid => return Err(RenderContextUnavailable::Invalid),
    };
    let frames = i64::try_from(info.frames).map_err(|_| RenderContextUnavailable::Stale)?;
    let end = info
        .clock_samples
        .0
        .checked_add(frames)
        .ok_or(RenderContextUnavailable::Stale)?;
    if context.sample_rate() != info.sample_rate
        || context.render_frames().start.get() != info.clock_samples.0
        || context.render_frames().end.get() != end
    {
        return Err(RenderContextUnavailable::Stale);
    }
    Ok(context)
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
        let processed = process_transport(info, events, &mut extra.store);
        let next = processed
            .as_ref()
            .ok()
            .and_then(|frame| build(info, frame))
            .map_or(RenderContextSlot::Invalid, RenderContextSlot::Ready);
        let invalid = matches!(next, RenderContextSlot::Invalid);
        let Some(slot) = extra.store.try_get_mut::<RenderContextSlot>() else {
            let _ = extra
                .logger
                .try_error("render context store slot is missing");
            return ProcessStatus::ClearAllOutputs;
        };
        *slot = next;
        if invalid {
            let message = processed
                .err()
                .map_or("render context is invalid", |error| error.message());
            let _ = extra.logger.try_error(message);
        }
        ProcessStatus::ClearAllOutputs
    }
}

fn build(info: &ProcInfo, transport: &TransportFrame) -> Option<RenderContext> {
    let render_frames = info.clock_samples_range();
    RenderContext::new(
        RenderFrame::new(render_frames.start.0)..RenderFrame::new(render_frames.end.0),
        info.sample_rate,
        transport.session_beats.clone(),
        transport.commit,
    )
}

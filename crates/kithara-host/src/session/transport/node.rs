use firewheel::{
    FirewheelContext,
    node::{
        AudioNode, AudioNodeInfo, AudioNodeProcessor, ConstructProcessorContext, EmptyConfig,
        NodeError, ProcBuffers, ProcExtra, ProcInfo, ProcStreamCtx, ProcessStatus,
    },
};
use kithara_command::{ScopedConfig, ScopedSender, scoped_channel};
use kithara_config::Config;
use kithara_render::{
    bridge::DeckProtocol,
    rt::{install_render_context, invalidate_render_context, publish_render_context},
};
use kithara_signal::{OutputContext, SessionFrame};
use kithara_test_utils::kithara;
use kithara_warp::RenderContext;
use triple_buffer::{Output, triple_buffer};

use super::{
    commit::{SessionGridGeneration, TransportObservation},
    process::{
        TransportFrame, TransportObservationInput, TransportState, process_transport,
        restart_transport,
    },
};
use crate::{host::HostSettings, session::queue::HostProtocol};

type InstalledTransport = (
    ScopedSender<HostProtocol, DeckProtocol>,
    Output<TransportObservation>,
);

pub(crate) fn install(
    ctx: &mut FirewheelContext,
    session_grid: SessionGridGeneration,
    settings: HostSettings,
    config: ScopedConfig,
) -> Result<InstalledTransport, &'static str> {
    let initial = TransportObservation::new(None, session_grid);
    let (observation_input, observation_output) = triple_buffer(&initial);
    let (channel, inbox) = scoped_channel::<HostProtocol, DeckProtocol>(config);
    let store = ctx
        .proc_store_mut()
        .ok_or("session transport store is unavailable while the stream is running")?;
    install_render_context(store)?;
    store
        .insert(TransportState::new(
            inbox,
            settings,
            session_grid,
            config.values().root.values().capacity.get(),
        ))
        .map_err(|_| "session transport state store slot already exists")?;
    store
        .insert(TransportObservationInput::new(observation_input))
        .map_err(|_| "session transport observation store slot already exists")?;
    ctx.add_node(SessionTransportNode, None)
        .map_err(|_| "session transport node was rejected by the audio graph")?;
    Ok((channel, observation_output))
}

pub(crate) struct SessionTransportNode;

impl AudioNode for SessionTransportNode {
    type Configuration = EmptyConfig;

    fn construct_processor(
        &self,
        _configuration: &Self::Configuration,
        _cx: ConstructProcessorContext,
    ) -> Result<impl AudioNodeProcessor, NodeError> {
        Ok(SessionTransportProcessor)
    }

    fn info(&self, _configuration: &Self::Configuration) -> Result<AudioNodeInfo, NodeError> {
        Ok(AudioNodeInfo::new()
            .debug_name("SessionTransport")
            .is_pre_process())
    }
}

pub(crate) struct SessionTransportProcessor;

impl AudioNodeProcessor for SessionTransportProcessor {
    #[kithara::rtsan_forbid_blocking]
    fn process(
        &mut self,
        info: &ProcInfo,
        _buffers: ProcBuffers,
        extra: &mut ProcExtra,
    ) -> ProcessStatus {
        let processed = process_transport(info, &mut extra.store);
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
}

fn build(info: &ProcInfo, transport: &TransportFrame) -> Option<RenderContext> {
    let output_frames = info.clock_samples_range();
    let output = OutputContext::new(
        SessionFrame::new(output_frames.start.0)..SessionFrame::new(output_frames.end.0),
        info.sample_rate,
        transport.session_epoch,
        Some(transport.transport_revision),
    )?;
    RenderContext::new(output, Some(transport.trajectory))
}

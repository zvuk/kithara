use std::{
    num::NonZeroU32,
    sync::atomic::{AtomicU32, AtomicUsize, Ordering},
};

use firewheel::{
    StreamInfo,
    channel_config::{ChannelConfig, ChannelCount},
    node::{
        AudioNode, AudioNodeInfo, AudioNodeProcessor, ConstructProcessorContext, EmptyConfig,
        NodeError, NodeID, ProcBuffers, ProcExtra, ProcInfo, ProcStreamCtx, ProcessStatus,
    },
};
use kithara_platform::sync::Arc;
use num_traits::AsPrimitive;

use crate::session::context::SessionGraph;

#[derive(Clone, Default)]
pub(crate) struct CountingProbe {
    inner: Arc<CountingProbeInner>,
}

#[derive(Default)]
struct CountingProbeInner {
    construction_sample_rate: AtomicU32,
    constructions: AtomicUsize,
    new_streams: AtomicUsize,
}

impl CountingProbe {
    pub(crate) fn construction_count(&self) -> usize {
        self.inner.constructions.load(Ordering::SeqCst)
    }

    pub(crate) fn construction_sample_rate(&self) -> Option<NonZeroU32> {
        NonZeroU32::new(self.inner.construction_sample_rate.load(Ordering::SeqCst))
    }

    pub(crate) fn new_stream_count(&self) -> usize {
        self.inner.new_streams.load(Ordering::SeqCst)
    }
}

#[derive(Clone, Default)]
pub(crate) struct FixtureNode<S> {
    state: S,
}

impl<S> FixtureNode<S> {
    #[must_use]
    pub(crate) const fn new(state: S) -> Self {
        Self { state }
    }
}

pub(crate) type CountingNode = FixtureNode<CountingProbe>;
pub(crate) type DeterministicToneNode = FixtureNode<()>;

trait FixtureState: Clone + Send + 'static {
    const DEBUG_NAME: &'static str;

    fn construct(&self, stream_info: &StreamInfo);
    fn new_stream(&mut self);
    fn process(&mut self, info: &ProcInfo, buffers: ProcBuffers) -> ProcessStatus;
}

impl<S> AudioNode for FixtureNode<S>
where
    S: FixtureState,
{
    type Configuration = EmptyConfig;

    fn construct_processor(
        &self,
        _configuration: &Self::Configuration,
        cx: ConstructProcessorContext,
    ) -> Result<impl AudioNodeProcessor, NodeError> {
        self.state.construct(cx.stream_info);
        Ok(FixtureProcessor(self.state.clone()))
    }

    fn info(&self, _configuration: &Self::Configuration) -> Result<AudioNodeInfo, NodeError> {
        Ok(AudioNodeInfo::new()
            .debug_name(S::DEBUG_NAME)
            .channel_config(ChannelConfig {
                num_inputs: ChannelCount::ZERO,
                num_outputs: ChannelCount::STEREO,
            }))
    }
}

struct FixtureProcessor<S>(S);

impl<S> AudioNodeProcessor for FixtureProcessor<S>
where
    S: FixtureState,
{
    fn new_stream(&mut self, _stream_info: &StreamInfo, _context: &mut ProcStreamCtx) {
        self.0.new_stream();
    }

    fn process(
        &mut self,
        info: &ProcInfo,
        buffers: ProcBuffers,
        _extra: &mut ProcExtra,
    ) -> ProcessStatus {
        self.0.process(info, buffers)
    }
}

impl FixtureState for CountingProbe {
    const DEBUG_NAME: &'static str = "ring_counting";

    fn construct(&self, stream_info: &StreamInfo) {
        self.inner
            .construction_sample_rate
            .store(stream_info.sample_rate.get(), Ordering::SeqCst);
        self.inner.constructions.fetch_add(1, Ordering::SeqCst);
    }

    fn new_stream(&mut self) {
        self.inner.new_streams.fetch_add(1, Ordering::SeqCst);
    }

    fn process(&mut self, info: &ProcInfo, buffers: ProcBuffers) -> ProcessStatus {
        for output in &mut *buffers.outputs {
            output[..info.frames].fill(0.0);
        }
        ProcessStatus::ClearAllOutputs
    }
}

impl FixtureState for () {
    const DEBUG_NAME: &'static str = "ring_deterministic_tone";

    fn construct(&self, _stream_info: &StreamInfo) {}

    fn new_stream(&mut self) {}

    fn process(&mut self, info: &ProcInfo, buffers: ProcBuffers) -> ProcessStatus {
        for frame in 0..info.frames {
            let absolute = info.clock_samples.0 + frame as i64;
            let phase: f32 = absolute.rem_euclid(64).as_();
            let sample = phase / 128.0 - 0.25;
            for output in &mut *buffers.outputs {
                output[frame] = sample;
            }
        }
        ProcessStatus::OutputsModified
    }
}

pub(crate) fn install_stereo_source<N>(ctx: &mut SessionGraph, node: N) -> Result<NodeID, String>
where
    N: AudioNode<Configuration = EmptyConfig> + 'static,
{
    let node_id = ctx
        .add_node(node, None)
        .map_err(|error| format!("audio graph rejected the ring fixture node: {error}"))?;
    let graph_out = ctx.graph_out_node_id();
    ctx.connect(node_id, graph_out, &[(0, 0), (1, 1)], false)
        .map_err(|error| format!("connect ring fixture to graph output failed: {error}"))?;
    Ok(node_id)
}

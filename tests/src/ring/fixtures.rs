use std::{
    num::NonZeroU32,
    sync::atomic::{AtomicU32, AtomicUsize, Ordering},
};

use firewheel::{
    FirewheelCtx, StreamInfo,
    channel_config::{ChannelConfig, ChannelCount},
    event::ProcEvents,
    node::{
        AudioNode, AudioNodeInfo, AudioNodeProcessor, ConstructProcessorContext, EmptyConfig,
        NodeID, ProcBuffers, ProcExtra, ProcInfo, ProcStreamCtx, ProcessStatus,
    },
};
use kithara::platform::sync::Arc;

use super::RingBackend;

#[derive(Clone, Default)]
pub struct CountingProbe {
    inner: Arc<CountingProbeInner>,
}

#[derive(Default)]
struct CountingProbeInner {
    constructions: AtomicUsize,
    construction_sample_rate: AtomicU32,
    new_streams: AtomicUsize,
}

impl CountingProbe {
    pub fn construction_count(&self) -> usize {
        self.inner.constructions.load(Ordering::SeqCst)
    }

    pub fn construction_sample_rate(&self) -> Option<NonZeroU32> {
        NonZeroU32::new(self.inner.construction_sample_rate.load(Ordering::SeqCst))
    }

    pub fn new_stream_count(&self) -> usize {
        self.inner.new_streams.load(Ordering::SeqCst)
    }
}

#[derive(Clone, Default)]
pub struct FixtureNode<S> {
    state: S,
}

impl<S> FixtureNode<S> {
    #[must_use]
    pub const fn new(state: S) -> Self {
        Self { state }
    }
}

pub type CountingNode = FixtureNode<CountingProbe>;
pub type DeterministicToneNode = FixtureNode<()>;

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
    ) -> impl AudioNodeProcessor {
        self.state.construct(cx.stream_info);
        FixtureProcessor(self.state.clone())
    }

    fn info(&self, _configuration: &Self::Configuration) -> AudioNodeInfo {
        AudioNodeInfo::new()
            .debug_name(S::DEBUG_NAME)
            .channel_config(ChannelConfig {
                num_inputs: ChannelCount::ZERO,
                num_outputs: ChannelCount::STEREO,
            })
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
        _events: &mut ProcEvents,
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
            let sample = absolute.rem_euclid(64) as f32 / 128.0 - 0.25;
            for output in &mut *buffers.outputs {
                output[frame] = sample;
            }
        }
        ProcessStatus::OutputsModified
    }
}

pub fn install_stereo_source<N>(
    ctx: &mut FirewheelCtx<RingBackend>,
    node: N,
) -> Result<NodeID, String>
where
    N: AudioNode<Configuration = EmptyConfig> + 'static,
{
    let node_id = ctx.add_node(node, None);
    let graph_out = ctx.graph_out_node_id();
    ctx.connect(node_id, graph_out, &[(0, 0), (1, 1)], false)
        .map_err(|error| format!("connect ring fixture to graph output failed: {error}"))?;
    Ok(node_id)
}

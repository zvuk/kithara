use core::{num::NonZeroUsize, sync::atomic::Ordering};

use firewheel::{
    StreamInfo,
    channel_config::{ChannelConfig, ChannelCount},
    event::ProcEvents,
    node::{
        AudioNode, AudioNodeInfo, AudioNodeProcessor, ConstructProcessorContext, EmptyConfig,
        ProcBuffers, ProcExtra, ProcInfo, ProcStreamCtx, ProcessStatus,
    },
};
use kithara_platform::sync::{Arc, Mutex};
use kithara_test_utils::kithara;
use ringbuf::traits::{Observer, Producer};

use crate::bridge::MixTapWriter;

/// Sink hanging off the session limiter beside `graph_out`: stereo in, no
/// outputs, the final mix copied out to one control-plane consumer.
pub(crate) struct TapNode {
    writer: Arc<Mutex<Option<MixTapWriter>>>,
}

impl TapNode {
    pub(crate) fn new(writer: MixTapWriter) -> Self {
        Self {
            writer: Arc::new(Mutex::new(Some(writer))),
        }
    }
}

impl AudioNode for TapNode {
    type Configuration = EmptyConfig;

    fn construct_processor(
        &self,
        _config: &Self::Configuration,
        cx: ConstructProcessorContext,
    ) -> impl AudioNodeProcessor {
        TapProcessor::new(self.writer.lock().take(), cx.stream_info)
    }

    fn info(&self, _config: &Self::Configuration) -> AudioNodeInfo {
        AudioNodeInfo::new()
            .debug_name("session_mix_tap")
            .channel_config(ChannelConfig {
                num_inputs: ChannelCount::STEREO,
                num_outputs: ChannelCount::ZERO,
            })
    }
}

struct TapProcessor {
    writer: Option<MixTapWriter>,
    interleaved: Vec<f32>,
}

impl TapProcessor {
    const STEREO: NonZeroUsize = NonZeroUsize::new(2).expect("2 is non-zero");

    fn new(writer: Option<MixTapWriter>, stream_info: &StreamInfo) -> Self {
        Self {
            writer,
            interleaved: Self::scratch(stream_info),
        }
    }

    fn scratch(stream_info: &StreamInfo) -> Vec<f32> {
        let frames = usize::try_from(stream_info.max_block_frames.get()).unwrap_or(usize::MAX);
        vec![0.0; frames.saturating_mul(Self::STEREO.get())]
    }
}

impl AudioNodeProcessor for TapProcessor {
    fn new_stream(&mut self, stream_info: &StreamInfo, _context: &mut ProcStreamCtx) {
        self.interleaved = Self::scratch(stream_info);
    }

    #[kithara::rtsan_forbid_blocking]
    fn process(
        &mut self,
        info: &ProcInfo,
        buffers: ProcBuffers,
        _events: &mut ProcEvents,
        _extra: &mut ProcExtra,
    ) -> ProcessStatus {
        let Some(ref mut writer) = self.writer else {
            return ProcessStatus::ClearAllOutputs;
        };
        let [left, right, ..] = buffers.inputs else {
            return ProcessStatus::ClearAllOutputs;
        };
        let stereo = Self::STEREO.get();
        // Frame-aligned pushes keep the consumer's channel order intact under
        // overflow: a lost half-frame would swap L and R for good.
        let frames = info
            .frames
            .min(self.interleaved.len() / stereo)
            .min(writer.pcm.vacant_len() / stereo);
        let chunk = &mut self.interleaved[..frames * stereo];
        for (frame, pair) in chunk.chunks_exact_mut(stereo).enumerate() {
            pair[0] = left[frame];
            pair[1] = right[frame];
        }
        let pushed = writer.pcm.push_slice(chunk);
        let dropped = u64::try_from(info.frames * stereo - pushed).unwrap_or(u64::MAX);
        if dropped > 0 {
            writer.drops.fetch_add(dropped, Ordering::Relaxed);
        }

        ProcessStatus::ClearAllOutputs
    }
}

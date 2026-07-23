use core::num::{NonZeroU32, NonZeroUsize};

use firewheel::{
    StreamInfo,
    channel_config::{ChannelConfig, ChannelCount},
    event::ProcEvents,
    mask::MaskType,
    node::{
        AudioNode, AudioNodeInfo, AudioNodeProcessor, ConstructProcessorContext, EmptyConfig,
        ProcBuffers, ProcExtra, ProcInfo, ProcStreamCtx, ProcessStatus,
    },
};
use kithara_audio::PeakLimiter;
use kithara_test_utils::kithara;

const SESSION_CEILING: f32 = 0.98;

const SESSION_RELEASE_MS: f32 = 50.0;

const STEREO: NonZeroUsize = NonZeroUsize::new(2).expect("2 is non-zero");

const _: () = assert!(SESSION_CEILING > 0.0 && SESSION_CEILING <= 1.0);
const _: () = assert!(SESSION_RELEASE_MS > 0.0);

/// Firewheel adapter around the shared [`PeakLimiter`]: the only site that sees
/// Firewheel buffers.
#[derive(Debug, Clone, Copy)]
pub(crate) struct LimiterNode;

impl AudioNode for LimiterNode {
    type Configuration = EmptyConfig;

    fn construct_processor(
        &self,
        _config: &Self::Configuration,
        cx: ConstructProcessorContext,
    ) -> impl AudioNodeProcessor {
        LimiterProcessor::new(cx.stream_info.sample_rate)
    }

    fn info(&self, _config: &Self::Configuration) -> AudioNodeInfo {
        AudioNodeInfo::new()
            .debug_name("session_limiter")
            .channel_config(ChannelConfig {
                num_inputs: ChannelCount::STEREO,
                num_outputs: ChannelCount::STEREO,
            })
    }
}

struct LimiterProcessor {
    limiter: PeakLimiter,
}

impl LimiterProcessor {
    fn new(sample_rate: NonZeroU32) -> Self {
        Self {
            limiter: build_limiter(sample_rate),
        }
    }
}

fn build_limiter(sample_rate: NonZeroU32) -> PeakLimiter {
    // The session constants are const-asserted valid above, so construction
    // cannot fail.
    PeakLimiter::new(sample_rate, STEREO, SESSION_CEILING, SESSION_RELEASE_MS)
        .expect("session limiter constants are const-asserted valid")
}

impl AudioNodeProcessor for LimiterProcessor {
    fn new_stream(&mut self, stream_info: &StreamInfo, _context: &mut ProcStreamCtx) {
        self.limiter = build_limiter(stream_info.sample_rate);
    }

    #[kithara::rtsan_forbid_blocking]
    fn process(
        &mut self,
        info: &ProcInfo,
        buffers: ProcBuffers,
        _events: &mut ProcEvents,
        _extra: &mut ProcExtra,
    ) -> ProcessStatus {
        if buffers.inputs.len() < STEREO.get() || buffers.outputs.len() < STEREO.get() {
            return ProcessStatus::Bypass;
        }

        buffers.outputs[0][..info.frames].copy_from_slice(&buffers.inputs[0][..info.frames]);
        buffers.outputs[1][..info.frames].copy_from_slice(&buffers.inputs[1][..info.frames]);

        if info.in_silence_mask.all_channels_silent(STEREO.get()) {
            return ProcessStatus::OutputsModifiedWithMask(MaskType::Silence(info.in_silence_mask));
        }

        let Some((out_l_slice, rest)) = buffers.outputs.split_first_mut() else {
            return ProcessStatus::Bypass;
        };
        let Some(out_r_slice) = rest.first_mut() else {
            return ProcessStatus::Bypass;
        };
        let mut channels: [&mut [f32]; 2] = [
            &mut out_l_slice[..info.frames],
            &mut out_r_slice[..info.frames],
        ];
        self.limiter.process_planar(&mut channels);

        ProcessStatus::OutputsModified
    }
}

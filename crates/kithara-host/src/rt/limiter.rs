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
use kithara_test_utils::kithara;

use crate::effects::{LimiterConfig, PeakLimiter};

/// Firewheel adapter around the shared [`PeakLimiter`]: the only site that sees
/// Firewheel buffers.
#[derive(Debug, Clone, Copy)]
pub(crate) struct LimiterNode {
    config: LimiterConfig,
}

impl LimiterNode {
    pub(crate) const fn new(config: LimiterConfig) -> Self {
        Self { config }
    }
}

impl AudioNode for LimiterNode {
    type Configuration = EmptyConfig;

    fn construct_processor(
        &self,
        _config: &Self::Configuration,
        cx: ConstructProcessorContext,
    ) -> impl AudioNodeProcessor {
        LimiterProcessor::new(cx.stream_info.sample_rate, self.config)
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
    config: LimiterConfig,
    limiter: PeakLimiter,
}

impl LimiterProcessor {
    const STEREO: NonZeroUsize = NonZeroUsize::new(2).expect("invariant: 2 is non-zero");

    fn new(sample_rate: NonZeroU32, config: LimiterConfig) -> Self {
        Self {
            config,
            limiter: Self::build_limiter(sample_rate, config),
        }
    }

    fn build_limiter(sample_rate: NonZeroU32, config: LimiterConfig) -> PeakLimiter {
        PeakLimiter::new(sample_rate, Self::STEREO, config)
            .expect("invariant: a stereo limiter fits the detector's channel limit")
    }
}

impl AudioNodeProcessor for LimiterProcessor {
    fn new_stream(&mut self, stream_info: &StreamInfo, _context: &mut ProcStreamCtx) {
        self.limiter = Self::build_limiter(stream_info.sample_rate, self.config);
    }

    #[kithara::rtsan_forbid_blocking]
    fn process(
        &mut self,
        info: &ProcInfo,
        buffers: ProcBuffers,
        _events: &mut ProcEvents,
        _extra: &mut ProcExtra,
    ) -> ProcessStatus {
        if buffers.inputs.len() < Self::STEREO.get() || buffers.outputs.len() < Self::STEREO.get() {
            return ProcessStatus::Bypass;
        }

        buffers.outputs[0][..info.frames].copy_from_slice(&buffers.inputs[0][..info.frames]);
        buffers.outputs[1][..info.frames].copy_from_slice(&buffers.inputs[1][..info.frames]);

        if info.in_silence_mask.all_channels_silent(Self::STEREO.get()) {
            // WHY: The limiter never sees this block, so its detector history would carry
            // the audio from before the silence into whatever starts after it. Telling it
            // the stream broke keeps the next onset judged as an onset.
            self.limiter.reset();
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

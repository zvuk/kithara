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
use tracing::warn;

const SESSION_CEILING: f32 = 0.98;

const SESSION_RELEASE_MS: f32 = 50.0;

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
    limiter: Option<PeakLimiter>,
}

impl LimiterProcessor {
    fn new(sample_rate: NonZeroU32) -> Self {
        Self {
            limiter: build_limiter(sample_rate),
        }
    }
}

fn build_limiter(sample_rate: NonZeroU32) -> Option<PeakLimiter> {
    let channels = NonZeroUsize::new(2)?;
    match PeakLimiter::new(sample_rate, channels, SESSION_CEILING, SESSION_RELEASE_MS) {
        Ok(limiter) => Some(limiter),
        Err(err) => {
            warn!(
                ?err,
                "session limiter config rejected; passing audio through"
            );
            None
        }
    }
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
        /// Minimum stereo channel count for processing.
        const MIN_STEREO: usize = 2;

        if buffers.inputs.len() < MIN_STEREO || buffers.outputs.len() < MIN_STEREO {
            return ProcessStatus::Bypass;
        }

        buffers.outputs[0][..info.frames].copy_from_slice(&buffers.inputs[0][..info.frames]);
        buffers.outputs[1][..info.frames].copy_from_slice(&buffers.inputs[1][..info.frames]);

        if info.in_silence_mask.all_channels_silent(MIN_STEREO) {
            return ProcessStatus::OutputsModifiedWithMask(MaskType::Silence(info.in_silence_mask));
        }

        let Some(limiter) = self.limiter.as_mut() else {
            return ProcessStatus::OutputsModified;
        };
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
        limiter.process_planar(&mut channels);

        ProcessStatus::OutputsModified
    }
}

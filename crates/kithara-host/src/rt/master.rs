use core::num::NonZeroU32;

use firewheel::{
    StreamInfo,
    channel_config::{ChannelConfig, ChannelCount},
    node::{
        AudioNode, AudioNodeInfo, AudioNodeProcessor, ConstructProcessorContext, EmptyConfig,
        NodeError, ProcBuffers, ProcExtra, ProcInfo, ProcStreamCtx, ProcessStatus,
    },
};
use kithara_dsp::param::{DEFAULT_SMOOTH_SECONDS, MIN_SETTLE_RATIO, SmoothedParam, SmootherConfig};
use kithara_test_utils::kithara;

use crate::{api::SessionDuckingMode, session::applied_spans};

mod consts {
    pub(super) const GAIN_CHUNK: usize = 256;
    /// A ducking amplitude lies between silence and unity.
    pub(super) const GAIN_SPAN: f32 = 1.0;
}

/// The first stage of the session output: it scales the mix by the amplitude
/// of the ducking volume the Host settings the render graph applied carry,
/// from the frame each applied on. The amplitude moves to each new ducking
/// along a 62 ms curve that settles at the finest step the filter supports,
/// so neither the change nor its end steps the signal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MasterNode;

impl AudioNode for MasterNode {
    type Configuration = EmptyConfig;

    fn construct_processor(
        &self,
        _config: &Self::Configuration,
        cx: ConstructProcessorContext,
    ) -> Result<impl AudioNodeProcessor, NodeError> {
        Ok(MasterProcessor {
            sample_rate: cx.stream_info.sample_rate,
            gain: None,
        })
    }

    fn info(&self, _config: &Self::Configuration) -> Result<AudioNodeInfo, NodeError> {
        Ok(AudioNodeInfo::new()
            .debug_name("session_master")
            .channel_config(ChannelConfig {
                num_inputs: ChannelCount::STEREO,
                num_outputs: ChannelCount::STEREO,
            }))
    }
}

struct MasterProcessor {
    sample_rate: NonZeroU32,
    /// None until the first block the settings reach.
    gain: Option<SmoothedParam>,
}

impl MasterProcessor {
    /// The amplitude, which starts at `ducking` on the first block.
    fn gain(&mut self, ducking: SessionDuckingMode) -> &mut SmoothedParam {
        let sample_rate = self.sample_rate;
        self.gain.get_or_insert_with(|| {
            SmoothedParam::new(
                ducking.volume().amp(),
                consts::GAIN_SPAN,
                SmootherConfig {
                    smooth_seconds: DEFAULT_SMOOTH_SECONDS,
                    settle_ratio: MIN_SETTLE_RATIO,
                },
                sample_rate,
            )
        })
    }
}

impl AudioNodeProcessor for MasterProcessor {
    fn new_stream(&mut self, stream_info: &StreamInfo, _context: &mut ProcStreamCtx) {
        self.sample_rate = stream_info.sample_rate;
        if let Some(gain) = &mut self.gain {
            gain.update_sample_rate(stream_info.sample_rate);
        }
    }

    #[kithara::rtsan_forbid_blocking]
    fn process(
        &mut self,
        info: &ProcInfo,
        buffers: ProcBuffers,
        extra: &mut ProcExtra,
    ) -> ProcessStatus {
        let frames = info.frames;
        let Some(spans) = applied_spans(&extra.store, frames) else {
            return ProcessStatus::Bypass;
        };
        let duckings = spans.map(|(range, span)| (range, span.settings().ducking()));
        let (Some((_, first)), Some((_, last))) =
            (duckings.clone().next(), duckings.clone().last())
        else {
            return ProcessStatus::Bypass;
        };
        let gain = self.gain(first);
        if info
            .in_silence_mask
            .all_channels_silent(ChannelCount::STEREO.get() as usize)
        {
            gain.set_value(last.volume().amp());
            gain.reset_to_target();
            return ProcessStatus::ClearAllOutputs;
        }
        if duckings
            .clone()
            .all(|(_, ducking)| ducking == SessionDuckingMode::Off)
        {
            gain.set_value(SessionDuckingMode::Off.volume().amp());
            if gain.has_settled() {
                return ProcessStatus::Bypass;
            }
        }
        let ([in_left, in_right, ..], [out_left, out_right, ..]) =
            (buffers.inputs, buffers.outputs)
        else {
            return ProcessStatus::Bypass;
        };
        let (Some(in_left), Some(in_right), Some(out_left), Some(out_right)) = (
            in_left.get(..frames),
            in_right.get(..frames),
            out_left.get_mut(..frames),
            out_right.get_mut(..frames),
        ) else {
            return ProcessStatus::Bypass;
        };
        let mut gains = [0.0; consts::GAIN_CHUNK];
        for (range, ducking) in duckings {
            gain.set_value(ducking.volume().amp());
            let (Some(in_left), Some(in_right), Some(out_left), Some(out_right)) = (
                in_left.get(range.clone()),
                in_right.get(range.clone()),
                out_left.get_mut(range.clone()),
                out_right.get_mut(range),
            ) else {
                continue;
            };
            for (((out_left, out_right), in_left), in_right) in out_left
                .chunks_mut(consts::GAIN_CHUNK)
                .zip(out_right.chunks_mut(consts::GAIN_CHUNK))
                .zip(in_left.chunks(consts::GAIN_CHUNK))
                .zip(in_right.chunks(consts::GAIN_CHUNK))
            {
                let gains = &mut gains[..out_left.len()];
                for level in gains.iter_mut() {
                    *level = gain.next_smoothed();
                }
                out_left.copy_from_slice(in_left);
                out_right.copy_from_slice(in_right);
                kithara_dsp::multiply_in_place(out_left, gains);
                kithara_dsp::multiply_in_place(out_right, gains);
            }
        }
        gain.settle();
        ProcessStatus::OutputsModified
    }
}

#[cfg(test)]
#[path = "master_tests.rs"]
mod tests;

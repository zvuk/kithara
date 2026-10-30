use core::num::{NonZeroU32, NonZeroUsize};

use firewheel::{
    StreamInfo,
    channel_config::{ChannelConfig, ChannelCount},
    mask::MaskType,
    node::{
        AudioNode, AudioNodeInfo, AudioNodeProcessor, ConstructProcessorContext, EmptyConfig,
        NodeError, ProcBuffers, ProcExtra, ProcInfo, ProcStreamCtx, ProcessStatus,
    },
};
use kithara_test_utils::kithara;

use crate::{LimiterConfig, PeakLimiter};

/// Firewheel adapter around the shared [`PeakLimiter`]: the only site that sees
/// Firewheel buffers.
#[derive(Debug, Clone, Copy)]
pub struct LimiterNode {
    config: LimiterConfig,
}

impl LimiterNode {
    /// Wrap `config` in a node the session graph can install.
    #[must_use]
    pub const fn new(config: LimiterConfig) -> Self {
        Self { config }
    }
}

impl AudioNode for LimiterNode {
    type Configuration = EmptyConfig;

    fn construct_processor(
        &self,
        _config: &Self::Configuration,
        cx: ConstructProcessorContext,
    ) -> Result<impl AudioNodeProcessor, NodeError> {
        Ok(LimiterProcessor::new(
            cx.stream_info.sample_rate,
            self.config,
        ))
    }

    fn info(&self, _config: &Self::Configuration) -> Result<AudioNodeInfo, NodeError> {
        Ok(AudioNodeInfo::new()
            .debug_name("session_limiter")
            .channel_config(ChannelConfig {
                num_inputs: ChannelCount::STEREO,
                num_outputs: ChannelCount::STEREO,
            }))
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
        _extra: &mut ProcExtra,
    ) -> ProcessStatus {
        if buffers.inputs.len() < Self::STEREO.get() || buffers.outputs.len() < Self::STEREO.get() {
            return ProcessStatus::Bypass;
        }

        buffers.outputs[0][..info.frames].copy_from_slice(&buffers.inputs[0][..info.frames]);
        buffers.outputs[1][..info.frames].copy_from_slice(&buffers.inputs[1][..info.frames]);

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

        if info.in_silence_mask.all_channels_silent(Self::STEREO.get()) {
            ProcessStatus::OutputsModifiedWithMask(MaskType::Silence(info.in_silence_mask))
        } else {
            ProcessStatus::OutputsModified
        }
    }
}

#[cfg(test)]
mod tests {
    use core::time::Duration;

    use firewheel::{
        clock::InstantSamples,
        dsp::{buffer::ConstSequentialBuffer, declick::DeclickValues},
        log::{RealtimeLoggerConfig, realtime_logger},
        mask::{ConnectedMask, ConstantMask, SilenceMask},
        node::{NUM_SCRATCH_BUFFERS, ProcStore, StreamStatus},
    };

    use super::*;

    #[kithara::test(native, flash(false))]
    fn silent_blocks_preserve_the_same_limiter_lifecycle() {
        const FRAMES: usize = 16;
        let rate = NonZeroU32::new(48_000).unwrap();
        let config = LimiterConfig::default();
        let mut processor = LimiterProcessor::new(rate, config);
        let mut reference = PeakLimiter::new(rate, LimiterProcessor::STEREO, config).unwrap();
        let (logger, _receiver) = realtime_logger(RealtimeLoggerConfig::default());
        let mut extra = ProcExtra {
            logger,
            store: ProcStore::with_capacity(0),
            scratch_buffers: ConstSequentialBuffer::<f32, NUM_SCRATCH_BUFFERS>::new(FRAMES),
            declick_values: DeclickValues::new(NonZeroU32::new(16).unwrap()),
        };
        let mut info = ProcInfo {
            sample_rate: rate,
            frames: FRAMES,
            in_silence_mask: SilenceMask::default(),
            out_silence_mask: SilenceMask::default(),
            in_constant_mask: ConstantMask::default(),
            out_constant_mask: ConstantMask::default(),
            in_connected_mask: ConnectedMask::default(),
            out_connected_mask: ConnectedMask::default(),
            total_cpu_seconds_recip: 1.0,
            process_to_playback_delay: None,
            did_just_unbypass: false,
            last_marker_instant: InstantSamples(0),
            sample_rate_recip: f64::from(rate.get()).recip(),
            clock_samples: InstantSamples(0),
            duration_since_stream_start: Duration::ZERO,
            stream_status: StreamStatus::empty(),
            dropped_frames: 0,
        };

        // Authored planar f32 at 48 kHz: attack, two silent blocks, then a quiet
        // onset. The adapter must preserve the owner's PCM exactly (zero tolerance).
        for (block, value) in [4.0, 0.0, 0.0, 0.25].into_iter().enumerate() {
            let left = [value; FRAMES];
            let right = [value * 0.5; FRAMES];
            let mut expected_left = left;
            let mut expected_right = right;
            reference.process_planar(&mut [&mut expected_left, &mut expected_right]);
            if block == 3 {
                assert!(expected_left[0] > 0.0 && expected_left[0] < value);
            }
            info.in_silence_mask = if value == 0.0 {
                SilenceMask::new_all_silent(2)
            } else {
                SilenceMask::default()
            };
            let mut actual_left = [0.0; FRAMES];
            let mut actual_right = [0.0; FRAMES];
            let status = processor.process(
                &info,
                ProcBuffers {
                    inputs: &[&left, &right],
                    outputs: &mut [&mut actual_left, &mut actual_right],
                },
                &mut extra,
            );
            assert_eq!(
                actual_left.map(f32::to_bits),
                expected_left.map(f32::to_bits),
                "left block {block}"
            );
            assert_eq!(
                actual_right.map(f32::to_bits),
                expected_right.map(f32::to_bits),
                "right block {block}"
            );
            let expected_status = if value == 0.0 {
                ProcessStatus::OutputsModifiedWithMask(MaskType::Silence(
                    SilenceMask::new_all_silent(2),
                ))
            } else {
                ProcessStatus::OutputsModified
            };
            assert_eq!(status, expected_status, "status block {block}");
        }
    }
}

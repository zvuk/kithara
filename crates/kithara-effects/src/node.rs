use core::{fmt::Write as _, num::NonZeroU32};

use firewheel::{
    StreamInfo,
    channel_config::{ChannelConfig, ChannelCount},
    mask::MaskType,
    node::{
        AudioNode, AudioNodeInfo, AudioNodeProcessor, ConstructProcessorContext, EmptyConfig,
        NodeError, ProcBuffers, ProcExtra, ProcInfo, ProcStreamCtx, ProcessStatus,
    },
};
use kithara_bufpool::{HasPool, PoolRegion};
use kithara_test_utils::kithara;

use crate::{LimiterConfig, LimiterError, PeakLimiter, consts::STEREO};

/// Firewheel adapter around the shared [`PeakLimiter`]: the only site that sees
/// Firewheel buffers.
#[derive_where::derive_where(Clone, Debug)]
pub struct LimiterNode<S> {
    config: LimiterConfig,
    max_rate: NonZeroU32,
    pools: PoolRegion<S>,
}

impl<S> LimiterNode<S> {
    /// Wrap `config` in a node the session graph can install. A processor takes its storage
    /// for every stream rate up to `max_rate` from `pools` once, when it is built.
    #[must_use]
    pub const fn new(config: LimiterConfig, max_rate: NonZeroU32, pools: PoolRegion<S>) -> Self {
        Self {
            config,
            max_rate,
            pools,
        }
    }
}

impl<S> AudioNode for LimiterNode<S>
where
    S: HasPool<f32> + Send + Sync + 'static,
{
    type Configuration = EmptyConfig;

    fn construct_processor(
        &self,
        _config: &Self::Configuration,
        cx: ConstructProcessorContext,
    ) -> Result<impl AudioNodeProcessor, NodeError> {
        LimiterProcessor::new(
            cx.stream_info.sample_rate,
            self.max_rate,
            self.config,
            &self.pools,
        )
        .map_err(|error| NodeError(Box::new(error)))
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
    limiter: PeakLimiter,
    /// Set while the stream rate needs more lookahead than the limiter holds: the
    /// output stays silent until a stream that fits.
    muted: bool,
}

impl LimiterProcessor {
    fn new<S: HasPool<f32>>(
        sample_rate: NonZeroU32,
        max_rate: NonZeroU32,
        config: LimiterConfig,
        pools: &PoolRegion<S>,
    ) -> Result<Self, LimiterError> {
        Ok(Self {
            limiter: PeakLimiter::new(sample_rate, max_rate, STEREO, config, pools)?,
            muted: false,
        })
    }
}

impl AudioNodeProcessor for LimiterProcessor {
    /// Retune in the storage the processor was built with; never takes from the region.
    fn new_stream(&mut self, stream_info: &StreamInfo, context: &mut ProcStreamCtx) {
        match self.limiter.retune(stream_info.sample_rate) {
            Ok(()) => self.muted = false,
            Err(error) => {
                if !self.muted {
                    let _ = context.logger.try_error_with(|line| {
                        let _ = write!(line, "the session limiter silences the output: {error}");
                    });
                }
                self.muted = true;
            }
        }
    }

    #[kithara::rtsan_forbid_blocking]
    fn process(
        &mut self,
        info: &ProcInfo,
        buffers: ProcBuffers,
        _extra: &mut ProcExtra,
    ) -> ProcessStatus {
        if self.muted {
            return ProcessStatus::ClearAllOutputs;
        }
        if buffers.inputs.len() < STEREO.get() || buffers.outputs.len() < STEREO.get() {
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

        // A silent input block still carries the delayed audio of a lookahead.
        if info.in_silence_mask.all_channels_silent(STEREO.get())
            && channels
                .iter()
                .all(|channel| channel.iter().all(|sample| *sample == 0.0))
        {
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
        log::{RealtimeLoggerConfig, RealtimeLoggerMainThread, realtime_logger},
        mask::{ConnectedMask, ConstantMask, SilenceMask},
        node::{NUM_SCRATCH_BUFFERS, ProcStore, StreamStatus},
    };

    use super::*;
    use crate::{
        LookaheadMs,
        test_pools::{pools, pools_with_budget},
    };

    const FRAMES: usize = 16;

    type Block = [f32; FRAMES];

    fn rate(hz: u32) -> NonZeroU32 {
        NonZeroU32::new(hz).unwrap()
    }

    fn lookahead(ms: f32) -> LimiterConfig {
        LimiterConfig::builder()
            .lookahead_ms(LookaheadMs::try_from(ms).unwrap())
            .build()
    }

    /// Firewheel's side of one processor: the block it hands over, the context a new
    /// stream brings, and the main-thread end of the realtime log.
    struct Rig {
        info: ProcInfo,
        extra: ProcExtra,
        log: RealtimeLoggerMainThread,
    }

    impl Rig {
        fn new(sample_rate: NonZeroU32) -> Self {
            let (logger, log) = realtime_logger(RealtimeLoggerConfig::default());
            Self {
                info: ProcInfo {
                    sample_rate,
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
                    sample_rate_recip: f64::from(sample_rate.get()).recip(),
                    clock_samples: InstantSamples(0),
                    duration_since_stream_start: Duration::ZERO,
                    stream_status: StreamStatus::empty(),
                    dropped_frames: 0,
                },
                extra: ProcExtra {
                    logger,
                    store: ProcStore::with_capacity(0),
                    scratch_buffers: ConstSequentialBuffer::<f32, NUM_SCRATCH_BUFFERS>::new(FRAMES),
                    declick_values: DeclickValues::new(NonZeroU32::new(16).unwrap()),
                },
                log,
            }
        }

        fn restart(&mut self, processor: &mut impl AudioNodeProcessor, sample_rate: NonZeroU32) {
            self.info.sample_rate = sample_rate;
            self.info.sample_rate_recip = f64::from(sample_rate.get()).recip();
            let stream = StreamInfo {
                sample_rate,
                ..StreamInfo::default()
            };
            processor.new_stream(
                &stream,
                &mut ProcStreamCtx {
                    store: &mut self.extra.store,
                    logger: &mut self.extra.logger,
                },
            );
        }

        /// One stereo block through `processor`, its input silence mask set from the PCM.
        fn process(
            &mut self,
            processor: &mut impl AudioNodeProcessor,
            left: &Block,
            right: &Block,
        ) -> (ProcessStatus, Block, Block) {
            self.info.in_silence_mask = if left.iter().chain(right).all(|&sample| sample == 0.0) {
                SilenceMask::new_all_silent(2)
            } else {
                SilenceMask::default()
            };
            let (mut out_left, mut out_right) = ([0.0; FRAMES], [0.0; FRAMES]);
            let status = processor.process(
                &self.info,
                ProcBuffers {
                    inputs: &[left, right],
                    outputs: &mut [&mut out_left, &mut out_right],
                },
                &mut self.extra,
            );
            (status, out_left, out_right)
        }

        fn errors(&mut self) -> Vec<String> {
            let mut errors = Vec::new();
            self.log
                .flush(|error| errors.push(error.to_owned()), |_debug| {});
            errors
        }
    }

    /// `processor` runs `blocks` exactly as a limiter freshly built at `sample_rate`.
    fn assert_runs_as_fresh(
        rig: &mut Rig,
        processor: &mut impl AudioNodeProcessor,
        sample_rate: NonZeroU32,
        max_rate: NonZeroU32,
        config: LimiterConfig,
        blocks: &[(f32, f32)],
    ) {
        let mut fresh = PeakLimiter::new(sample_rate, max_rate, STEREO, config, &pools()).unwrap();
        for (index, &(left, right)) in blocks.iter().enumerate() {
            let (left, right) = ([left; FRAMES], [right; FRAMES]);
            let (mut expected_left, mut expected_right) = (left, right);
            fresh.process_planar(&mut [&mut expected_left, &mut expected_right]);
            let (status, actual_left, actual_right) = rig.process(processor, &left, &right);
            assert_ne!(status, ProcessStatus::ClearAllOutputs, "block {index}");
            assert_eq!(
                actual_left.map(f32::to_bits),
                expected_left.map(f32::to_bits),
                "left block {index}"
            );
            assert_eq!(
                actual_right.map(f32::to_bits),
                expected_right.map(f32::to_bits),
                "right block {index}"
            );
        }
    }

    #[kithara::test(native, flash(false))]
    fn silent_blocks_preserve_the_same_limiter_lifecycle() {
        let rate = rate(48_000);
        let config = LimiterConfig::default();
        let mut processor = LimiterProcessor::new(rate, rate, config, &pools()).unwrap();
        let mut reference = PeakLimiter::new(rate, rate, STEREO, config, &pools()).unwrap();
        let mut rig = Rig::new(rate);

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
            let (status, actual_left, actual_right) = rig.process(&mut processor, &left, &right);
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

    #[kithara::test]
    fn silent_input_blocks_carry_the_delayed_audio_until_it_drains() {
        // 0.5 ms at 48 kHz trails by 24 frames: the sounding block leaves over the next two
        // silent-input blocks, and only the third is silence again.
        let rate = rate(48_000);
        let config = lookahead(0.5);
        assert_eq!(config.latency_frames(rate), 24);
        let mut processor = LimiterProcessor::new(rate, rate, config, &pools()).unwrap();
        let mut reference = PeakLimiter::new(rate, rate, STEREO, config, &pools()).unwrap();
        let mut rig = Rig::new(rate);
        let silence = ProcessStatus::OutputsModifiedWithMask(MaskType::Silence(
            SilenceMask::new_all_silent(2),
        ));
        let mut sounding = Vec::new();
        for (block, (value, expected_status)) in [
            (0.5, ProcessStatus::OutputsModified),
            (0.0, ProcessStatus::OutputsModified),
            (0.0, ProcessStatus::OutputsModified),
            (0.0, silence),
        ]
        .into_iter()
        .enumerate()
        {
            let (left, right) = ([value; FRAMES], [value * 0.5; FRAMES]);
            let (mut expected_left, mut expected_right) = (left, right);
            reference.process_planar(&mut [&mut expected_left, &mut expected_right]);
            let (status, actual_left, actual_right) = rig.process(&mut processor, &left, &right);
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
            assert_eq!(status, expected_status, "status block {block}");
            sounding.extend(
                (0..FRAMES)
                    .filter(|&frame| actual_left[frame] != 0.0)
                    .map(|frame| block * FRAMES + frame),
            );
        }
        assert_eq!(sounding, (24..24 + FRAMES).collect::<Vec<_>>());
    }

    #[kithara::test]
    fn a_restart_takes_nothing_from_a_region_holding_exactly_the_reserve() {
        // 0.25 ms at the highest rate, 96 kHz, trails by 24 frames: one f32 gain target each,
        // and each delayed plane holds those frames and one 256-frame chunk. Every rate trails
        // by less than a block, so a restart that kept delayed audio would sound it.
        const RESERVE: usize = (24 + 2 * (24 + 256)) * size_of::<f32>();
        let max_rate = rate(96_000);
        let config = lookahead(0.25);
        let region = pools_with_budget(RESERVE);
        let mut processor = LimiterProcessor::new(rate(48_000), max_rate, config, &region).unwrap();
        let mut rig = Rig::new(rate(48_000));
        let (status, ..) = rig.process(&mut processor, &[4.0; FRAMES], &[2.0; FRAMES]);
        assert_eq!(status, ProcessStatus::OutputsModified);
        for restart in [48_000, 96_000, 44_100] {
            rig.restart(&mut processor, rate(restart));
            assert_runs_as_fresh(
                &mut rig,
                &mut processor,
                rate(restart),
                max_rate,
                config,
                &[(4.0, 2.0), (0.0, 0.0), (0.25, 0.125)],
            );
        }
        assert_eq!(rig.errors(), Vec::<String>::new());
    }

    #[kithara::test]
    fn a_stream_past_the_prepared_lookahead_is_silent_until_one_fits() {
        // 0.25 ms trails by 12 frames at 48 kHz and 11 at 44.1 kHz, less than a block.
        let prepared = rate(48_000);
        let config = lookahead(0.25);
        let mut processor = LimiterProcessor::new(prepared, prepared, config, &pools()).unwrap();
        let mut rig = Rig::new(prepared);
        for above in [96_000, 192_000] {
            rig.restart(&mut processor, rate(above));
            let (status, ..) = rig.process(&mut processor, &[0.5; FRAMES], &[0.25; FRAMES]);
            assert_eq!(status, ProcessStatus::ClearAllOutputs, "{above} Hz");
        }
        let errors = rig.errors();
        assert_eq!(
            errors.len(),
            1,
            "one line while the output stays silent: {errors:?}"
        );
        assert!(errors[0].contains("96000 Hz"), "{errors:?}");
        let fits = rate(44_100);
        rig.restart(&mut processor, fits);
        assert_runs_as_fresh(
            &mut rig,
            &mut processor,
            fits,
            prepared,
            config,
            &[(4.0, 2.0), (0.25, 0.125)],
        );
        assert_eq!(rig.errors(), Vec::<String>::new());
    }
}

use std::num::NonZeroU32;

use bungee_sys::InputChunk;
use kithara_bufpool::HasPool;
use kithara_signal::{AudioSpec, FrameCount, PlanarBuffer, SignalError};
use kithara_test_macros as kithara;
use num_traits::ToPrimitive;

use super::ffi::{AnalysisInput, NativeStretcher};
use crate::{ElasticConfig, ElasticError};

pub(super) fn planar_buffer<S>(
    config: &ElasticConfig<S>,
    frames: usize,
) -> Result<PlanarBuffer, ElasticError>
where
    S: HasPool<f32>,
{
    let channels = u16::try_from(config.channels())
        .map_err(|_| ElasticError::ChannelCountOutOfRange(config.channels()))?;
    let sample_rate =
        NonZeroU32::new(config.sample_rate()).ok_or(ElasticError::InvalidSampleRate)?;
    let mut buffer = PlanarBuffer::new(
        config.pools(),
        AudioSpec::new(channels, sample_rate),
        FrameCount::new(frames),
    )
    .map_err(signal_error)?;
    buffer.shrink_to_fit();
    Ok(buffer)
}

fn frames_between(begin: i32, end: i32) -> Result<usize, ElasticError> {
    let frames = end
        .checked_sub(begin)
        .ok_or(ElasticError::SampleCountOverflow)?;
    usize::try_from(frames).map_err(|_| ElasticError::SampleCountOverflow)
}

pub(super) fn signal_error(error: SignalError) -> ElasticError {
    match error {
        SignalError::ChannelCountZero => ElasticError::InvalidChannelCount,
        SignalError::SampleCountOverflow { .. } => ElasticError::SampleCountOverflow,
        SignalError::PoolCapacity { .. } => ElasticError::PoolCapacity,
        SignalError::Shape { .. }
        | SignalError::IncompleteFrame { .. }
        | SignalError::FrameRange { .. }
        | SignalError::ChannelRange { .. }
        | SignalError::ChannelCount { .. }
        | SignalError::ChannelFrames { .. }
        | SignalError::Capacity { .. }
        | SignalError::DurationOverflow { .. }
        | SignalError::FrameCountOverflow { .. } => {
            ElasticError::EnginePreparation("Bungee planar buffer invariant failed")
        }
        _ => ElasticError::EnginePreparation("Bungee planar buffer invariant failed"),
    }
}

#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in)]
pub(super) struct InputBuffer {
    #[field(set, vis = "pub(super)")]
    requested: InputChunk,
    analysis: PlanarBuffer,
    audio: PlanarBuffer,
    begin: i32,
    #[field(get, copy, vis = "pub(super)")]
    end: i32,
}

impl InputBuffer {
    pub(super) fn new<S>(
        config: &ElasticConfig<S>,
        max_input_frames: usize,
        max_source_frames: usize,
    ) -> Result<Self, ElasticError>
    where
        S: HasPool<f32>,
    {
        let capacity = max_input_frames
            .checked_add(max_source_frames)
            .ok_or(ElasticError::SampleCountOverflow)?;
        Ok(Self {
            analysis: planar_buffer(config, max_input_frames)?,
            audio: planar_buffer(config, capacity)?,
            begin: 0,
            end: 0,
            requested: InputChunk { begin: 0, end: 0 },
        })
    }

    pub(super) fn analyse(
        &mut self,
        native: &mut NativeStretcher,
        valid: bool,
        end_of_input: bool,
    ) -> Result<(), ElasticError> {
        kithara::measure_block!("bungee::analysis::clear", {
            self.analysis.as_samples_mut().fill(0.0);
        });
        if !valid {
            return native.analyse(AnalysisInput {
                samples: self.analysis.as_samples(),
                channel_stride: self.analysis.stride().get(),
                mute_head: 0,
                mute_tail: 0,
            });
        }

        let requested_frames = self.requested_frames()?;
        if requested_frames > self.analysis.stride().get() {
            return Err(ElasticError::EnginePreparation(
                "Bungee requested an oversized input grain",
            ));
        }
        let available_begin = self.requested.begin.max(self.begin);
        let available_end = self.requested.end.min(self.end).max(available_begin);
        let copied = frames_between(available_begin, available_end)?;
        if copied > 0 {
            self.copy_available(available_begin, copied)?;
        }
        let mute_head =
            usize::try_from((i64::from(self.begin) - i64::from(self.requested.begin)).max(0))
                .map_err(|_| ElasticError::SampleCountOverflow)?
                .min(requested_frames);
        let mute_tail =
            usize::try_from((i64::from(self.requested.end) - i64::from(self.end)).max(0))
                .map_err(|_| ElasticError::SampleCountOverflow)?
                .min(requested_frames);
        if mute_tail > 0 && mute_head < requested_frames && !end_of_input {
            return Err(ElasticError::EnginePreparation(
                "Bungee requested unavailable future input",
            ));
        }
        native.analyse(AnalysisInput {
            mute_head,
            mute_tail,
            samples: self.analysis.as_samples(),
            channel_stride: self.analysis.stride().get(),
        })
    }

    pub(super) fn append(
        &mut self,
        source: Option<&[f32]>,
        input_frames: usize,
    ) -> Result<(), ElasticError> {
        let input_frames_i32 = i32::try_from(input_frames)
            .map_err(|_| ElasticError::SourceFrameLimitOutOfRange(input_frames))?;
        let discard = self.retain_requested(input_frames)?;

        let buffered = usize::try_from(self.end - self.begin)
            .map_err(|_| ElasticError::SampleCountOverflow)?;
        let appended = input_frames - discard;
        let required = buffered
            .checked_add(appended)
            .ok_or(ElasticError::SampleCountOverflow)?;
        let capacity = self.audio.stride().get();
        if required > capacity {
            return Err(ElasticError::InputStorage { required, capacity });
        }
        let channels = usize::from(self.audio.spec().channels);
        for channel in 0..channels {
            let destination =
                &mut self.audio.channel_mut(channel).map_err(signal_error)?[buffered..required];
            if let Some(source) = source {
                for (destination, frame) in destination.iter_mut().zip(discard..input_frames) {
                    *destination = source[frame * channels + channel];
                }
            } else {
                destination.fill(0.0);
            }
        }
        self.begin = self
            .begin
            .checked_add(i32::try_from(discard).map_err(|_| ElasticError::SampleCountOverflow)?)
            .ok_or(ElasticError::SampleCountOverflow)?;
        self.end = self
            .end
            .checked_add(input_frames_i32)
            .ok_or(ElasticError::SampleCountOverflow)?;
        Ok(())
    }

    pub(super) fn clear(&mut self) {
        self.set_position(0);
    }

    fn copy_available(&mut self, available_begin: i32, copied: usize) -> Result<(), ElasticError> {
        let source_begin = frames_between(self.begin, available_begin)?;
        let destination_begin = frames_between(self.requested.begin, available_begin)?;
        let channels = usize::from(self.audio.spec().channels);
        kithara::measure_block!("bungee::analysis::copy", {
            for channel in 0..channels {
                let source = &self.audio.channel(channel).map_err(signal_error)?
                    [source_begin..source_begin + copied];
                self.analysis.channel_mut(channel).map_err(signal_error)?
                    [destination_begin..destination_begin + copied]
                    .copy_from_slice(source);
            }
        });
        Ok(())
    }

    pub(super) fn prepare_source_capacity(&mut self, capacity: usize) -> Result<(), ElasticError> {
        self.audio
            .resize_frames(FrameCount::new(capacity))
            .map_err(signal_error)?;
        self.audio.shrink_to_fit();
        Ok(())
    }

    fn requested_frames(&self) -> Result<usize, ElasticError> {
        usize::try_from(
            self.requested
                .end
                .checked_sub(self.requested.begin)
                .ok_or(ElasticError::SampleCountOverflow)?,
        )
        .map_err(|_| ElasticError::EnginePreparation("Bungee requested an invalid input grain"))
    }

    pub(super) fn requested_window(&self, position: f64) -> Result<(usize, usize), ElasticError> {
        let position = position
            .to_i32()
            .filter(|converted| f64::from(*converted) == position)
            .ok_or(ElasticError::EnginePreparation(
                "Bungee reported an invalid processing center",
            ))?;
        let history = position
            .checked_sub(self.requested.begin)
            .and_then(|frames| usize::try_from(frames).ok());
        let lookahead = self
            .requested
            .end
            .checked_sub(position)
            .and_then(|frames| usize::try_from(frames).ok());
        history
            .zip(lookahead)
            .ok_or(ElasticError::EnginePreparation(
                "Bungee reported an input window outside its processing center",
            ))
    }

    fn retain_requested(&mut self, input_frames: usize) -> Result<usize, ElasticError> {
        let mut discard = 0;
        if self.requested.begin < self.end {
            if self.begin < self.requested.begin {
                let shift = usize::try_from(self.requested.begin - self.begin)
                    .map_err(|_| ElasticError::SampleCountOverflow)?;
                let retained = usize::try_from(self.end - self.begin)
                    .map_err(|_| ElasticError::SampleCountOverflow)?;
                let channels = usize::from(self.audio.spec().channels);
                for channel in 0..channels {
                    self.audio
                        .channel_mut(channel)
                        .map_err(signal_error)?
                        .copy_within(shift..retained, 0);
                }
                self.begin = self.requested.begin;
            }
        } else {
            let skipped = self
                .requested
                .begin
                .checked_sub(self.begin)
                .ok_or(ElasticError::SampleCountOverflow)?;
            discard = usize::try_from(skipped)
                .map_err(|_| ElasticError::SampleCountOverflow)?
                .min(input_frames);
            self.begin = self.end;
        }
        Ok(discard)
    }

    pub(super) fn set_position(&mut self, position: i32) {
        self.begin = position;
        self.end = position;
        self.requested = InputChunk {
            begin: position,
            end: position,
        };
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{consts, test_pools::pools};

    #[kithara::test]
    fn an_append_behind_the_requested_window_keeps_only_requested_source() {
        const INPUT_FRAMES: usize = 8;
        const FIRST_REQUESTED: usize = 3;

        let config = ElasticConfig::builder()
            .pools(pools())
            .sample_rate(consts::SAMPLE_RATE)
            .channels(consts::CHANNELS)
            .max_source_frames(consts::CONTEXT_FRAMES)
            .max_output_frames(consts::CONTEXT_FRAMES)
            .build()
            .expect("the fixture shape is valid");
        let mut buffer = InputBuffer::new(&config, consts::CONTEXT_FRAMES, consts::CONTEXT_FRAMES)
            .expect("the fixture buffer prepares");
        let begin = i32::try_from(FIRST_REQUESTED).expect("the requested frame fits i32");
        let end = i32::try_from(FIRST_REQUESTED + INPUT_FRAMES).expect("the window end fits i32");
        buffer.set_requested(InputChunk { begin, end });
        let source: Vec<f32> = (0..INPUT_FRAMES * consts::CHANNELS)
            .map(|sample| f32::from(u16::try_from(sample).expect("the sample index fits u16")))
            .collect();

        buffer
            .append(Some(&source), INPUT_FRAMES)
            .expect("the append fits the buffer");

        for channel in 0..consts::CHANNELS {
            let expected: Vec<f32> = (FIRST_REQUESTED..INPUT_FRAMES)
                .map(|frame| source[frame * consts::CHANNELS + channel])
                .collect();
            let buffered = buffer.audio.channel(channel).expect("the fixture channel");
            assert_eq!(&buffered[..expected.len()], expected.as_slice());
        }
        assert_eq!(buffer.begin, begin);
    }
}

use kithara_test_macros as kithara;
use num_traits::ToPrimitive;

use super::{buffer::signal_error, stream::StreamCore};
use crate::{ElasticError, ElasticRequest};

impl StreamCore {
    pub(super) fn begin_request(
        &mut self,
        request: ElasticRequest,
        pitch: f64,
    ) -> Result<(f64, f64, usize), ElasticError> {
        let input = request
            .source_frames()
            .to_f64()
            .ok_or(ElasticError::SampleCountOverflow)?;
        let output_count = request
            .output_frames()
            .to_f64()
            .ok_or(ElasticError::SampleCountOverflow)?;
        let rate = input / output_count;
        self.request.speed = rate;
        self.request.pitch = pitch;
        self.samples_needed += output_count;
        let target = self
            .samples_needed
            .round()
            .to_usize()
            .ok_or(ElasticError::SampleCountOverflow)?;
        Ok((input, output_count, target))
    }

    pub(super) fn consume(
        &mut self,
        wanted: usize,
        output: Option<&mut [f32]>,
        output_frame: usize,
    ) -> Result<usize, ElasticError> {
        let Some(chunk) = self.output_chunk.as_ref().filter(|chunk| chunk.valid) else {
            return Ok(0);
        };
        let frames = wanted.min(chunk.frames.saturating_sub(self.output_consumed));
        if let Some(output) = output {
            let channels = usize::from(self.output.spec().channels);
            let destination_begin = output_frame
                .checked_mul(channels)
                .ok_or(ElasticError::SampleCountOverflow)?;
            let destination_end = frames
                .checked_mul(channels)
                .and_then(|samples| destination_begin.checked_add(samples))
                .ok_or(ElasticError::SampleCountOverflow)?;
            let actual = output.len();
            let destination = output.get_mut(destination_begin..destination_end).ok_or(
                ElasticError::OutputSampleCount {
                    actual,
                    expected: destination_end,
                },
            )?;
            self.output
                .view()
                .range(self.output_consumed..self.output_consumed + frames)
                .map_err(signal_error)?
                .interleave_into(destination)
                .map_err(signal_error)?;
        }
        self.output_consumed += frames;
        Ok(frames)
    }

    pub(super) fn position(
        &self,
        input_end: i32,
        input_frames: f64,
        output_frames: f64,
        remaining_output_frames: f64,
    ) -> Result<f64, ElasticError> {
        let rate = input_frames / output_frames;
        let scheduling_offset = self.rate_aware_offset(rate)?;
        Ok(f64::from(input_end)
            - scheduling_offset
            - input_frames * remaining_output_frames / output_frames)
    }

    pub(super) fn probe_silence(&mut self, request: ElasticRequest) -> Result<(), ElasticError> {
        self.render_inner(None, request, 1.0, None, true)
    }

    fn rate_aware_offset(&self, rate: f64) -> Result<f64, ElasticError> {
        let native_center = (self.max_input_frames() / 2)
            .to_f64()
            .ok_or(ElasticError::SampleCountOverflow)?;
        let source_latency = self
            .source_latency_frames
            .to_f64()
            .ok_or(ElasticError::SampleCountOverflow)?;
        Ok(source_latency + rate * (native_center - source_latency))
    }

    pub(super) fn render(
        &mut self,
        source: Option<&[f32]>,
        request: ElasticRequest,
        pitch: f64,
        output: Option<&mut [f32]>,
    ) -> Result<(), ElasticError> {
        self.render_inner(source, request, pitch, output, false)
    }

    #[cfg_attr(test, kithara::hang_watchdog)]
    fn render_anchored(
        &mut self,
        target: usize,
        mut output: Option<&mut [f32]>,
        end_of_input: bool,
    ) -> Result<(), ElasticError> {
        let anchor = self.anchor.ok_or(ElasticError::EnginePreparation(
            "Bungee anchored render has no source position",
        ))?;
        let iteration_limit = target
            .checked_add(Self::PIPELINE_GRAINS + 1)
            .ok_or(ElasticError::SampleCountOverflow)?;
        let mut rendered = 0;
        let mut stalled = 0usize;
        for _ in 0..iteration_limit {
            #[cfg(test)]
            hang_tick!();
            let output_consumed = self.output_consumed;
            self.discard_before(anchor)?;
            let discarded = self.output_consumed > output_consumed;
            let consumed = self.consume(target - rendered, output.as_deref_mut(), rendered)?;
            rendered += consumed;
            if rendered == target {
                #[cfg(test)]
                hang_reset!();
                self.samples_needed -=
                    rendered.to_f64().ok_or(ElasticError::SampleCountOverflow)?;
                return Ok(());
            }
            self.schedule_anchored(end_of_input)?;
            let produced = self
                .output_chunk
                .as_ref()
                .is_some_and(|chunk| chunk.valid && chunk.frames > self.output_consumed);
            if discarded || consumed > 0 || produced {
                #[cfg(test)]
                hang_reset!();
                stalled = 0;
            } else {
                stalled += 1;
                if stalled > Self::PIPELINE_GRAINS {
                    return Err(ElasticError::EnginePreparation(
                        "Bungee anchored output stopped advancing",
                    ));
                }
            }
        }
        Err(ElasticError::EnginePreparation(
            "Bungee anchored render exceeded its fixed iteration bound",
        ))
    }

    fn render_inner(
        &mut self,
        source: Option<&[f32]>,
        request: ElasticRequest,
        pitch: f64,
        output: Option<&mut [f32]>,
        end_of_input: bool,
    ) -> Result<(), ElasticError> {
        let result = (|| {
            self.input.append(source, request.source_frames())?;
            let (_, _, target) = self.begin_request(request, pitch)?;
            if self.anchor.is_some() {
                self.render_anchored(target, output, end_of_input)
            } else {
                self.render_target(
                    self.input.end(),
                    request,
                    target,
                    output,
                    end_of_input,
                    |_| Ok(()),
                )
            }
        })();
        if let Err(error) = result {
            self.recover()?;
            return Err(error);
        }
        Ok(())
    }

    #[cfg_attr(test, kithara::hang_watchdog)]
    pub(super) fn render_target<F>(
        &mut self,
        input_end: i32,
        request: ElasticRequest,
        target: usize,
        mut output: Option<&mut [f32]>,
        end_of_input: bool,
        mut prepare_input: F,
    ) -> Result<(), ElasticError>
    where
        F: FnMut(&mut Self) -> Result<(), ElasticError>,
    {
        let input = request
            .source_frames()
            .to_f64()
            .ok_or(ElasticError::SampleCountOverflow)?;
        let output_count = request
            .output_frames()
            .to_f64()
            .ok_or(ElasticError::SampleCountOverflow)?;
        let iteration_limit = target
            .checked_add(Self::PIPELINE_GRAINS + 1)
            .ok_or(ElasticError::SampleCountOverflow)?;
        let mut rendered = 0;
        let mut stalled = 0usize;
        for _ in 0..iteration_limit {
            #[cfg(test)]
            hang_tick!();
            let consumed = self.consume(target - rendered, output.as_deref_mut(), rendered)?;
            rendered += consumed;
            self.unprimed_started |= consumed > 0;
            if rendered == target {
                #[cfg(test)]
                hang_reset!();
                self.samples_needed -=
                    rendered.to_f64().ok_or(ElasticError::SampleCountOverflow)?;
                return Ok(());
            }
            let remaining = (target - rendered)
                .to_f64()
                .ok_or(ElasticError::SampleCountOverflow)?;
            let position = self.position(input_end, input, output_count, remaining)?;
            self.schedule_unprimed(position)?;
            self.input.set_requested(self.native.specify(&self.request));
            self.request_pending = true;
            prepare_input(self)?;
            self.synthesise(true, end_of_input)?;
            let produced = self
                .output_chunk
                .as_ref()
                .is_some_and(|chunk| chunk.valid && chunk.frames > self.output_consumed);
            if consumed > 0 || produced {
                #[cfg(test)]
                hang_reset!();
                stalled = 0;
            } else {
                stalled += 1;
                if stalled > Self::PIPELINE_GRAINS {
                    return Err(ElasticError::EnginePreparation(
                        "Bungee output stopped advancing",
                    ));
                }
            }
        }
        Err(ElasticError::EnginePreparation(
            "Bungee render exceeded its fixed iteration bound",
        ))
    }

    pub(super) fn schedule_anchored(&mut self, end_of_input: bool) -> Result<(), ElasticError> {
        if self.cue_grain_pending {
            self.cue_grain_pending = false;
        } else {
            self.native.next(&mut self.request);
        }
        self.input.set_requested(self.native.specify(&self.request));
        self.request_pending = true;
        self.synthesise(true, end_of_input)
    }

    fn schedule_unprimed(&mut self, desired_position: f64) -> Result<(), ElasticError> {
        if !self.unprimed_started {
            self.request.reset = u8::from(
                !self.request.position.is_finite() || desired_position <= self.request.position,
            );
            self.request.position = desired_position;
            return Ok(());
        }

        let previous_position = self.request.position;
        let requested_rate = self.request.speed;
        self.native.next(&mut self.request);
        let unit_hop = (self.request.position - previous_position) / requested_rate;
        if !previous_position.is_finite() || !unit_hop.is_finite() || unit_hop <= 0.0 {
            return Err(ElasticError::EnginePreparation(
                "Bungee reported an invalid running grain hop",
            ));
        }
        let minimum_position =
            previous_position + unit_hop * self.rate_envelope.min_source_frames_per_output();
        let maximum_position =
            previous_position + unit_hop * self.rate_envelope.max_source_frames_per_output();
        self.request.position = desired_position.clamp(minimum_position, maximum_position);
        self.request.reset = 0;
        Ok(())
    }

    #[kithara::measure]
    pub(super) fn synthesise(
        &mut self,
        valid: bool,
        end_of_input: bool,
    ) -> Result<(), ElasticError> {
        if !self.request_pending {
            return Err(ElasticError::EnginePreparation(
                "Bungee synthesis has no specified input grain",
            ));
        }
        self.input.analyse(&mut self.native, valid, end_of_input)?;
        self.request_pending = false;
        let output_stride = self.output.stride().get();
        self.output_chunk = Some(
            self.native
                .synthesise(self.output.as_samples_mut(), output_stride)?,
        );
        self.output_consumed = 0;
        Ok(())
    }
}

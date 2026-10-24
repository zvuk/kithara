use num_traits::ToPrimitive;

use super::stream::StreamCore;
use crate::{ElasticError, ElasticRequest};

impl StreamCore {
    pub(super) fn discard_before(&mut self, source_position: f64) -> Result<(), ElasticError> {
        let Some(chunk) = self
            .output_chunk
            .as_ref()
            .filter(|chunk| chunk.valid && chunk.end.is_finite() && chunk.frames > 0)
        else {
            return Ok(());
        };
        let frames = chunk
            .frames
            .to_f64()
            .ok_or(ElasticError::SampleCountOverflow)?;
        let discard = if chunk.end <= chunk.begin {
            chunk.frames
        } else {
            ((source_position - chunk.begin) * frames / (chunk.end - chunk.begin))
                .ceil()
                .clamp(0.0, frames)
                .to_usize()
                .ok_or(ElasticError::SampleCountOverflow)?
        };
        self.output_consumed = self.output_consumed.max(discard);
        Ok(())
    }

    fn discard_pre_cue(
        &mut self,
        output: &mut [f32],
        target_frames: usize,
        discarded_frames: &mut usize,
    ) -> Result<(), ElasticError> {
        let Some((end, frames)) = self.output_chunk.as_ref().and_then(|chunk| {
            (chunk.valid && chunk.end.is_finite() && chunk.frames > 0)
                .then_some((chunk.end, chunk.frames))
        }) else {
            return Ok(());
        };
        if end > 0.0 {
            return Err(ElasticError::EnginePreparation(
                "Bungee preroll produced post-cue output",
            ));
        }
        let remaining = target_frames.saturating_sub(*discarded_frames);
        let copied = self.consume(remaining.min(frames), Some(output), *discarded_frames)?;
        *discarded_frames = discarded_frames
            .checked_add(copied)
            .ok_or(ElasticError::SampleCountOverflow)?;
        self.output_consumed = frames;
        Ok(())
    }

    fn load_prime_input(
        &mut self,
        source_history: &[f32],
        source_lookahead: &[f32],
        request: ElasticRequest,
        source: &[f32],
    ) -> Result<(), ElasticError> {
        let channels = usize::from(self.output.spec().channels);
        let history_frames = source_history
            .len()
            .checked_div(channels)
            .ok_or(ElasticError::SampleCountOverflow)?;
        let history_frames_i32 = i32::try_from(history_frames)
            .map_err(|_| ElasticError::SourceFrameLimitOutOfRange(history_frames))?;
        self.input.set_position(
            history_frames_i32
                .checked_neg()
                .ok_or(ElasticError::SampleCountOverflow)?,
        );
        self.input.append(Some(source_history), history_frames)?;
        self.input.append(Some(source_lookahead), history_frames)?;
        self.input.append(Some(source), request.source_frames())?;
        Ok(())
    }

    pub(super) fn prime(
        &mut self,
        source_history: &[f32],
        source_lookahead: &[f32],
        request: ElasticRequest,
        source: &[f32],
        pitch: f64,
        discarded_output: &mut [f32],
    ) -> Result<(), ElasticError> {
        self.discard()?;
        self.load_prime_input(source_history, source_lookahead, request, source)?;
        let grain_count = self.schedule_preroll(request, pitch)?;

        let mut discarded_frames = 0;
        for grain in 0..grain_count {
            self.input.set_requested(self.native.specify(&self.request));
            self.request_pending = true;
            self.synthesise(true, false)?;
            self.discard_pre_cue(
                discarded_output,
                request.output_frames(),
                &mut discarded_frames,
            )?;
            if grain + 1 < grain_count {
                self.native.next(&mut self.request);
            }
        }
        if discarded_frames != request.output_frames() {
            return Err(ElasticError::EnginePreparation(
                "Bungee preroll did not produce the declared discard span",
            ));
        }

        self.native.next(&mut self.request);
        self.request.position = 0.0;
        self.request.reset = 0;
        self.output_chunk = None;
        self.output_consumed = 0;
        self.request_pending = false;
        self.samples_needed = 0.0;
        self.anchor = Some(0.0);
        self.audible_source_end = 0;
        self.audible_output_end = 0;
        self.grain_output_position = 0.0;
        self.cue_grain_pending = true;
        Ok(())
    }

    fn schedule_preroll(
        &mut self,
        request: ElasticRequest,
        pitch: f64,
    ) -> Result<usize, ElasticError> {
        let playback_rate = request.source_frames_per_output()?;
        self.request.position = 0.0;
        self.request.speed = playback_rate;
        self.request.pitch = pitch;
        self.request.reset = 0;
        self.native.preroll(&mut self.request);
        let input_hop = -self.request.position;
        let output_hop = input_hop / playback_rate;
        if !input_hop.is_finite() || input_hop <= 0.0 || !output_hop.is_finite() {
            return Err(ElasticError::EnginePreparation(
                "Bungee preroll reported an invalid grain hop",
            ));
        }
        let output_frames = request
            .output_frames()
            .to_f64()
            .ok_or(ElasticError::SampleCountOverflow)?;
        let discard_grains = (output_frames / output_hop)
            .ceil()
            .to_usize()
            .ok_or(ElasticError::SampleCountOverflow)?;
        let grain_count = discard_grains
            .checked_add(Self::PIPELINE_GRAINS)
            .ok_or(ElasticError::SampleCountOverflow)?;
        let grain_limit = request
            .output_frames()
            .checked_add(Self::PIPELINE_GRAINS)
            .ok_or(ElasticError::SampleCountOverflow)?;
        if grain_count > grain_limit {
            return Err(ElasticError::EnginePreparation(
                "Bungee preroll exceeded its output-derived grain bound",
            ));
        }
        let grain_count_f64 = grain_count
            .to_f64()
            .ok_or(ElasticError::SampleCountOverflow)?;
        self.request.position = -input_hop * grain_count_f64;
        self.request.reset = 1;
        Ok(grain_count)
    }
}

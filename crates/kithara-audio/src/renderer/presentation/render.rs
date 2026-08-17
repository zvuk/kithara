use kithara_decode::{DecodeError, DecodeResult, PcmChunk};

use super::{
    PRESENTATION_FRAMES,
    coordinator::Presentation,
    frontier::PresentationBarrier,
    output::PresentResult,
    state::{Discontinuity, DiscontinuityPhase, RawItem},
};
use crate::traits::{OutputCredit, TempoStage, TempoStep};

impl Presentation {
    pub(crate) fn step<F>(&mut self, mut retire: F) -> DecodeResult<PresentResult>
    where
        F: FnMut(PcmChunk),
    {
        if let Some(error) = self.buffer_error.take() {
            return Err(error);
        }
        if self.rejected_output.is_some() {
            return Err(DecodeError::InvalidData {
                detail: "presentation rejected output awaits off-RT retirement",
            });
        }
        if !self.output.has_capacity() {
            return Ok(PresentResult::Backpressured);
        }
        let Some(mut tempo) = self.tempo.take() else {
            return self.step_identity(&mut retire);
        };
        let result = self.step_tempo(tempo.as_mut(), &mut retire);
        self.tempo = Some(tempo);
        result
    }

    fn step_identity<F>(&mut self, retire: &mut F) -> DecodeResult<PresentResult>
    where
        F: FnMut(PcmChunk),
    {
        if let Some(RawItem::Barrier(PresentationBarrier::DecoderReplaced { spec, .. })) =
            self.raw.front()
            && self.buffers.spec != Some(*spec)
        {
            return Ok(PresentResult::Backpressured);
        }
        if matches!(self.raw.front(), Some(RawItem::Barrier(_))) {
            let barrier = self.take_barrier()?;
            return Ok(self.apply_barrier(barrier));
        }
        if self.raw.front().is_some() {
            let Some((chunk, epoch)) = self.next_block(retire)? else {
                return Ok(PresentResult::Backpressured);
            };
            let chunk = self.window.admit(chunk);
            let output = self.process_effects(chunk)?;
            return self.commit(output, epoch, 0);
        }
        self.finish_terminal(None, retire)
    }

    fn step_tempo<F>(
        &mut self,
        tempo: &mut dyn TempoStage,
        retire: &mut F,
    ) -> DecodeResult<PresentResult>
    where
        F: FnMut(PcmChunk),
    {
        if self.discontinuity.is_some() {
            return self.render_discontinuity(tempo, retire);
        }
        let buffered = Self::checked_tempo_quanta(tempo)?;
        if let Some(boundary) = tempo.prepared_boundary() {
            let barrier = if buffered == 0 && matches!(self.raw.front(), Some(RawItem::Barrier(_)))
            {
                Some(self.take_barrier()?)
            } else {
                None
            };
            self.discontinuity = Some(Discontinuity {
                barrier,
                boundary,
                phase: DiscontinuityPhase::Draining(tempo.begin_discontinuity()?),
            });
            return Ok(PresentResult::Advanced);
        }
        if buffered == 1 {
            return self.render_tempo(tempo, retire);
        }
        if matches!(self.raw.front(), Some(RawItem::Barrier(_))) {
            return Ok(PresentResult::Advanced);
        }
        if self.raw.front().is_some() {
            let chunk = self.take_source_chunk(retire)?;
            let chunk = self.window.admit(chunk);
            tempo.push_source(chunk)?;
            if Self::checked_tempo_quanta(tempo)? != 1 {
                return Err(DecodeError::InvalidData {
                    detail: "tempo stage did not retain its admitted source chunk",
                });
            }
            return Ok(PresentResult::Advanced);
        }
        self.finish_terminal(Some(tempo), retire)
    }

    fn render_tempo<F>(
        &mut self,
        tempo: &mut dyn TempoStage,
        retire: &mut F,
    ) -> DecodeResult<PresentResult>
    where
        F: FnMut(PcmChunk),
    {
        let spec = tempo.output_spec();
        let Some((mut pcm, channels)) = self.output_buffer(spec)? else {
            return Ok(PresentResult::Backpressured);
        };
        let step = match tempo.render(
            self.publisher.point(self.epoch),
            OutputCredit::new(&mut pcm, channels, PRESENTATION_FRAMES),
            retire,
        ) {
            Ok(step) => step,
            Err(error) => {
                self.recycle_pcm(spec, pcm)?;
                return Err(error);
            }
        };
        match step {
            TempoStep::Preparing => {
                self.recycle_pcm(spec, pcm)?;
                Ok(PresentResult::Advanced)
            }
            TempoStep::Consumed => {
                self.recycle_pcm(spec, pcm)?;
                if Self::checked_tempo_quanta(tempo)? != 1 {
                    return Err(DecodeError::InvalidData {
                        detail: "tempo stage lost its partially consumed source chunk",
                    });
                }
                Ok(PresentResult::Advanced)
            }
            TempoStep::NeedSource => {
                self.recycle_pcm(spec, pcm)?;
                if Self::checked_tempo_quanta(tempo)? != 0 {
                    return Err(DecodeError::InvalidData {
                        detail: "tempo stage requested source while retaining a chunk",
                    });
                }
                Ok(PresentResult::Advanced)
            }
            TempoStep::Rendered { frames, meta } => {
                let chunk = self.tempo_chunk(pcm, spec, channels, frames, meta)?;
                let output = self.process_effects(chunk)?;
                self.commit(output, self.epoch, tempo.held_source_frames())
            }
        }
    }
}

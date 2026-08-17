use kithara_decode::{DecodeError, DecodeResult, PcmChunk};

use super::{
    PRESENTATION_FRAMES,
    coordinator::Presentation,
    frontier::PresentationBarrier,
    output::PresentResult,
    state::{DiscontinuityPhase, RawItem, Terminal},
};
use crate::{
    pipeline::fetch::Fetch,
    traits::{OutputCredit, TempoDiscontinuityStep, TempoEofStep, TempoStage},
};

impl Presentation {
    pub(super) fn render_tempo_eof<F>(
        &mut self,
        tempo: &mut dyn TempoStage,
        epoch: u64,
        retire: &mut F,
    ) -> DecodeResult<PresentResult>
    where
        F: FnMut(PcmChunk),
    {
        let spec = tempo.output_spec();
        let Some((mut pcm, channels)) = self.output_buffer(spec)? else {
            return Ok(PresentResult::Backpressured);
        };
        let debt = self.eof_debt.as_mut().ok_or(DecodeError::InvalidData {
            detail: "tempo EOF drain lost its debt token",
        })?;
        let step = match tempo.render_eof(
            debt,
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
            TempoEofStep::Drained => {
                self.recycle_pcm(spec, pcm)?;
                self.eof_debt = None;
                tempo.deactivate(retire)?;
                self.push_terminal(Fetch::eof(epoch))?;
                self.terminal_sent = true;
                Ok(PresentResult::Terminal)
            }
            TempoEofStep::Rendered { frames, meta } => {
                let chunk = self.tempo_chunk(pcm, spec, channels, frames, meta)?;
                let output = self.process_effects(chunk)?;
                self.commit(output, epoch, tempo.held_source_frames())
            }
        }
    }

    pub(super) fn render_discontinuity<F>(
        &mut self,
        tempo: &mut dyn TempoStage,
        retire: &mut F,
    ) -> DecodeResult<PresentResult>
    where
        F: FnMut(PcmChunk),
    {
        let draining = matches!(
            self.discontinuity
                .as_ref()
                .map(|discontinuity| &discontinuity.phase),
            Some(DiscontinuityPhase::Draining(_))
        );
        if draining {
            let phase = {
                let discontinuity =
                    self.discontinuity
                        .as_mut()
                        .ok_or(DecodeError::InvalidData {
                            detail: "tempo discontinuity lost its debt token",
                        })?;
                std::mem::replace(&mut discontinuity.phase, DiscontinuityPhase::Drained)
            };
            let DiscontinuityPhase::Draining(mut debt) = phase else {
                return Err(DecodeError::InvalidData {
                    detail: "tempo discontinuity changed phase while rendering",
                });
            };
            let spec = tempo.output_spec();
            let Some((mut pcm, channels)) = self.output_buffer(spec)? else {
                self.discontinuity
                    .as_mut()
                    .ok_or(DecodeError::InvalidData {
                        detail: "tempo discontinuity disappeared during backpressure",
                    })?
                    .phase = DiscontinuityPhase::Draining(debt);
                return Ok(PresentResult::Backpressured);
            };
            let step = match tempo.render_discontinuity(
                &mut debt,
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
                TempoDiscontinuityStep::Drained => {
                    self.recycle_pcm(spec, pcm)?;
                    if tempo.held_source_frames() != 0 && Self::checked_tempo_quanta(tempo)? == 0 {
                        return Err(DecodeError::InvalidData {
                            detail: "tempo discontinuity drained with unowned source remaining",
                        });
                    }
                    let discontinuity =
                        self.discontinuity
                            .as_mut()
                            .ok_or(DecodeError::InvalidData {
                                detail: "tempo discontinuity disappeared before commit",
                            })?;
                    discontinuity.phase = DiscontinuityPhase::Drained;
                }
                TempoDiscontinuityStep::Rendered { frames, meta } => {
                    let Some(discontinuity) = self.discontinuity.as_mut() else {
                        self.recycle_pcm(spec, pcm)?;
                        return Err(DecodeError::InvalidData {
                            detail: "tempo discontinuity disappeared while rendering",
                        });
                    };
                    discontinuity.phase = DiscontinuityPhase::Draining(debt);
                    let chunk = self.tempo_chunk(pcm, spec, channels, frames, meta)?;
                    let output = self.process_effects(chunk)?;
                    return self.commit(output, self.epoch, tempo.held_source_frames());
                }
            }
        }

        let discontinuity = self.discontinuity.take().ok_or(DecodeError::InvalidData {
            detail: "tempo discontinuity disappeared before commit",
        })?;
        let barrier = discontinuity.barrier;
        if let Some(PresentationBarrier::DecoderReplaced {
            spec: replacement, ..
        }) = barrier
            && self.buffers.spec != Some(replacement)
        {
            self.discontinuity = Some(discontinuity);
            return Ok(PresentResult::Backpressured);
        }
        tempo.commit_prepared(discontinuity.boundary)?;
        if let Some(barrier) = barrier {
            let PresentationBarrier::DecoderReplaced {
                spec: replacement, ..
            } = barrier;
            if tempo.output_spec() != replacement {
                return Err(DecodeError::InvalidData {
                    detail: "tempo stage did not adopt the replacement decoder PCM shape",
                });
            }
            return Ok(self.apply_barrier(barrier));
        }
        Ok(PresentResult::Advanced)
    }

    pub(super) fn finish_terminal<F>(
        &mut self,
        tempo: Option<&mut dyn TempoStage>,
        retire: &mut F,
    ) -> DecodeResult<PresentResult>
    where
        F: FnMut(PcmChunk),
    {
        let Some(terminal) = self.terminal else {
            return Ok(PresentResult::Idle);
        };
        if self.terminal_sent {
            return Ok(PresentResult::Idle);
        }
        match terminal {
            Terminal::Eof { epoch } => {
                let Some(tempo) = tempo else {
                    self.push_terminal(Fetch::eof(epoch))?;
                    self.terminal_sent = true;
                    return Ok(PresentResult::Terminal);
                };
                if self.eof_debt.is_none() {
                    self.eof_debt = Some(tempo.finish_eof()?);
                    return Ok(PresentResult::Advanced);
                }
                self.render_tempo_eof(tempo, epoch, retire)
            }
            Terminal::Failed { epoch } => {
                if !self.failure_reset {
                    if let Some(tempo) = tempo {
                        tempo.deactivate(retire)?;
                    }
                    self.reset_effects();
                    self.window.clear();
                    self.failure_reset = true;
                }
                self.push_terminal(Fetch::failure(epoch))?;
                self.terminal_sent = true;
                Ok(PresentResult::Terminal)
            }
        }
    }

    pub(super) fn take_barrier(&mut self) -> DecodeResult<PresentationBarrier> {
        let Some(RawItem::Barrier(barrier)) = self.raw.pop_front() else {
            return Err(DecodeError::InvalidData {
                detail: "presentation decoder barrier disappeared",
            });
        };
        Ok(barrier)
    }

    pub(super) fn apply_barrier(&mut self, barrier: PresentationBarrier) -> PresentResult {
        let PresentationBarrier::DecoderReplaced { epoch, .. } = barrier;
        let source_end = self
            .window
            .emitted(0)
            .map(|source_end| (source_end.frame, source_end.rate));
        self.reset_effects();
        self.epoch = epoch;
        self.publisher.restart(epoch, source_end);
        PresentResult::Advanced
    }
}

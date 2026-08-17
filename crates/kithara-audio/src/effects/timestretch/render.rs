use kithara_decode::{DecodeError, DecodeResult, PcmChunk};

use super::{TimeStretchProcessor, state::AdmittedSource};
use crate::traits::{OutputCredit, TempoStep};

impl TimeStretchProcessor {
    pub(super) fn render_admitted(
        &mut self,
        mut credit: OutputCredit<'_>,
        retire: &mut dyn FnMut(PcmChunk),
    ) -> DecodeResult<TempoStep> {
        self.validate_credit(&mut credit)?;
        let Some(mut admitted) = self.admitted.take() else {
            return Ok(TempoStep::NeedSource);
        };
        match self.render_admitted_inner(&mut admitted, &mut credit) {
            Ok((step, consumed)) => {
                admitted.consumed_frames = admitted.consumed_frames.checked_add(consumed).ok_or(
                    DecodeError::InvalidData {
                        detail: "tempo admitted source cursor overflow",
                    },
                )?;
                let consumed_chunk = admitted.consumed_frames == admitted.chunk.frames();
                if consumed_chunk {
                    retire(admitted.chunk);
                    return Ok(step);
                }
                if matches!(step, TempoStep::Rendered { .. } | TempoStep::Preparing) {
                    self.admitted = Some(admitted);
                    return Ok(step);
                }
                if consumed == 0 {
                    self.admitted = Some(admitted);
                    self.failed = true;
                    return Err(DecodeError::InvalidData {
                        detail: "tempo backend made no source or output progress",
                    });
                }
                self.admitted = Some(admitted);
                Ok(TempoStep::Consumed)
            }
            Err(error) => {
                self.admitted = Some(admitted);
                self.failed = true;
                Err(error)
            }
        }
    }

    fn render_unity(
        &mut self,
        admitted: &AdmittedSource,
        credit: &mut OutputCredit<'_>,
        remaining: usize,
    ) -> DecodeResult<(TempoStep, usize)> {
        let frames = remaining.min(credit.max_frames());
        let channels = self.channels();
        let start =
            admitted
                .consumed_frames
                .checked_mul(channels)
                .ok_or(DecodeError::InvalidData {
                    detail: "tempo unity source offset overflow",
                })?;
        let samples = frames
            .checked_mul(channels)
            .ok_or(DecodeError::InvalidData {
                detail: "tempo unity sample count overflow",
            })?;
        let end = start.checked_add(samples).ok_or(DecodeError::InvalidData {
            detail: "tempo unity source range overflow",
        })?;
        credit.samples_mut()[..samples].copy_from_slice(
            admitted
                .chunk
                .samples
                .get(start..end)
                .ok_or(DecodeError::InvalidData {
                    detail: "tempo unity source range exceeds PCM",
                })?,
        );
        let meta = self.output_meta(&admitted.chunk.meta, admitted.consumed_frames, frames)?;
        self.last_input_meta = Some(admitted.chunk.meta);
        Ok((TempoStep::Rendered { frames, meta }, frames))
    }

    fn remaining_source(&self, admitted: &AdmittedSource) -> DecodeResult<usize> {
        if admitted.chunk.spec() != self.active()?.spec {
            return Err(DecodeError::InvalidData {
                detail: "tempo stage received a source with a different PCM spec",
            });
        }
        admitted
            .chunk
            .frames()
            .checked_sub(admitted.consumed_frames)
            .ok_or(DecodeError::InvalidData {
                detail: "tempo source cursor exceeds the admitted chunk",
            })
    }

    fn fitting_source_frames(
        &self,
        available: usize,
        credit_samples: usize,
    ) -> DecodeResult<usize> {
        let backend = self.active()?.backend()?;
        let mut lower = 1;
        let mut upper = available;
        let mut best = 0;
        while lower <= upper {
            let middle = lower.midpoint(upper);
            if backend.max_output_samples(middle) <= credit_samples {
                best = middle;
                lower = middle + 1;
            } else {
                upper = middle - 1;
            }
        }
        Ok(best)
    }
    fn render_admitted_inner(
        &mut self,
        admitted: &mut AdmittedSource,
        credit: &mut OutputCredit<'_>,
    ) -> DecodeResult<(TempoStep, usize)> {
        let remaining = self.remaining_source(admitted)?;
        let speed = self.active()?.snapshot.speed.max(Self::MIN_SPEED);
        if remaining == 0 {
            return Ok((TempoStep::NeedSource, 0));
        }
        if self.active()?.bypass() {
            return self.render_unity(admitted, credit, remaining);
        }

        let source_frame = admitted
            .chunk
            .meta
            .frame_offset
            .checked_add(u64::try_from(admitted.consumed_frames).map_err(|_| {
                DecodeError::InvalidData {
                    detail: "tempo source cursor exceeds u64",
                }
            })?)
            .ok_or(DecodeError::InvalidData {
                detail: "tempo source position overflow",
            })?;
        let (region, crossed) = self.region_for(source_frame)?;
        if crossed {
            self.request_region_boundary(source_frame)?;
            return Ok((TempoStep::Preparing, 0));
        }
        let region_remaining =
            region
                .end()
                .checked_sub(source_frame)
                .ok_or(DecodeError::InvalidData {
                    detail: "tempo region ends before the source cursor",
                })?;
        let remaining_u64 = u64::try_from(remaining).map_err(|_| DecodeError::InvalidData {
            detail: "tempo admitted source length exceeds u64",
        })?;
        let region_frames = if region_remaining >= remaining_u64 {
            remaining
        } else {
            usize::try_from(region_remaining).map_err(|_| DecodeError::InvalidData {
                detail: "tempo region length exceeds usize",
            })?
        };
        let available = remaining
            .min(region_frames.max(1))
            .min(Self::PRESENTATION_FRAMES);
        let base = 1.0 / f64::from(speed);
        let pitch = if self.active()?.snapshot.keylock {
            1.0
        } else {
            f64::from(speed)
        };
        self.apply_pitch(pitch)?;
        self.apply_stretch(base * region.correction())?;
        let credit_samples =
            credit
                .max_frames()
                .checked_mul(self.channels())
                .ok_or(DecodeError::InvalidData {
                    detail: "tempo credit sample count overflow",
                })?;
        let source_frames = self.fitting_source_frames(available, credit_samples)?;
        if source_frames == 0 {
            return Err(DecodeError::InvalidData {
                detail: "tempo backend cannot fit one source frame in the output credit",
            });
        }
        let channels = self.channels();
        let source_range = admitted.sample_range(source_frames, channels)?;
        let predicted = self.active()?.backend()?.max_output_samples(source_frames);
        if predicted > self.active()?.scratch.capacity() || predicted > credit_samples {
            return Err(DecodeError::InvalidData {
                detail: "tempo backend output exceeds its prepared credit bound",
            });
        }
        let input = admitted
            .chunk
            .samples
            .get(source_range)
            .ok_or(DecodeError::InvalidData {
                detail: "tempo source range exceeds admitted PCM",
            })?;
        let core = self.active_mut()?;
        core.scratch.clear();
        let backend = core
            .backend
            .as_deref_mut()
            .ok_or(DecodeError::InvalidData {
                detail: "active tempo core lost its DSP backend",
            })?;
        backend
            .process(input, &mut core.scratch)
            .map_err(|_| DecodeError::InvalidData {
                detail: "time-stretch backend failed to render",
            })?;
        let rendered_samples = self.active()?.scratch.len();
        if rendered_samples > predicted
            || rendered_samples > credit_samples
            || rendered_samples % channels != 0
        {
            self.active_mut()?.scratch.clear();
            return Err(DecodeError::InvalidData {
                detail: "tempo backend violated its declared output bound",
            });
        }
        let core = self.active_mut()?;
        core.processing = true;
        core.source_frames_pushed = core
            .source_frames_pushed
            .checked_add(
                u64::try_from(source_frames).map_err(|_| DecodeError::InvalidData {
                    detail: "tempo source progress exceeds u64",
                })?,
            )
            .ok_or(DecodeError::InvalidData {
                detail: "tempo source progress overflow",
            })?;
        let output_frames = rendered_samples / channels;
        if output_frames == 0 {
            return Ok((TempoStep::NeedSource, source_frames));
        }
        credit.samples_mut()[..rendered_samples].copy_from_slice(&self.active()?.scratch);
        self.active_mut()?.scratch.clear();
        let meta = self.output_meta(
            &admitted.chunk.meta,
            admitted.consumed_frames,
            output_frames,
        )?;
        self.last_input_meta = Some(admitted.chunk.meta);
        Ok((
            TempoStep::Rendered {
                frames: output_frames,
                meta,
            },
            source_frames,
        ))
    }
}

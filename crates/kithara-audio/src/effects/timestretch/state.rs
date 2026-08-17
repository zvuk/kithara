use std::ops::Range;

use kithara_decode::{DecodeError, DecodeResult, PcmChunk, PcmMeta, PcmSpec};
use kithara_stretch::{DrainDisposition, StretchBackend};

use super::{super::backend::StretchSnapshot, TimeStretchProcessor};
use crate::{
    region::ActiveRegion,
    traits::{OutputCredit, TempoBoundaryId},
};

pub(super) struct TempoCore {
    pub(super) backend: Option<Box<dyn StretchBackend>>,
    pub(super) scratch: Vec<f32>,
    pub(super) snapshot: StretchSnapshot,
    pub(super) spec: PcmSpec,
    pub(super) region: Option<ActiveRegion>,
    pub(super) applied_pitch: f64,
    pub(super) applied_stretch: f64,
    pub(super) source_frames_pushed: u64,
    pub(super) processing: bool,
}

pub(super) struct PreparedCore {
    pub(super) cause: PrepareCause,
    pub(super) id: TempoBoundaryId,
    pub(super) core: TempoCore,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum PrepareCause {
    Current,
    DecoderBoundary,
    RegionBoundary,
}

#[derive(Clone, Copy)]
pub(super) struct RegionBoundary {
    pub(super) source_frame: u64,
}

pub(super) struct RetiredCores {
    pub(super) active: Option<TempoCore>,
    pub(super) prepared: Option<PreparedCore>,
}

impl TimeStretchProcessor {
    pub(super) fn apply_pitch(&mut self, pitch: f64) -> DecodeResult<()> {
        let core = self.active_mut()?;
        if !core.applied_pitch.is_nan() && (pitch - core.applied_pitch).abs() <= Self::RATIO_EPS {
            return Ok(());
        }
        core.backend_mut()?
            .set_pitch(pitch)
            .map_err(|_| DecodeError::InvalidData {
                detail: "time-stretch backend rejected pitch",
            })?;
        core.applied_pitch = pitch;
        Ok(())
    }

    pub(super) fn apply_stretch(&mut self, stretch: f64) -> DecodeResult<()> {
        let core = self.active_mut()?;
        if !core.applied_stretch.is_nan()
            && (stretch - core.applied_stretch).abs() <= Self::RATIO_EPS
        {
            return Ok(());
        }
        core.backend_mut()?
            .set_ratio(stretch)
            .map_err(|_| DecodeError::InvalidData {
                detail: "time-stretch backend rejected ratio",
            })?;
        core.applied_stretch = stretch;
        Ok(())
    }

    pub(super) fn update_active_snapshot(&mut self, snapshot: StretchSnapshot) -> DecodeResult<()> {
        let core = self.active_mut()?;
        let plan_changed = !Self::same_plan(&core.snapshot.plan, &snapshot.plan);
        let parameters_changed = core.snapshot.keylock != snapshot.keylock
            || (core.snapshot.speed - snapshot.speed).abs() > f32::EPSILON;
        core.snapshot = snapshot;
        if plan_changed {
            core.region = None;
        }
        if parameters_changed || plan_changed {
            core.applied_pitch = f64::NAN;
            core.applied_stretch = f64::NAN;
        }
        Ok(())
    }

    pub(super) fn region_for(&mut self, frame: u64) -> DecodeResult<(ActiveRegion, bool)> {
        let core = self.active_mut()?;
        if let Some(r) = core.region
            && r.contains(frame)
        {
            return Ok((r, false));
        }
        let mut next = self
            .active()?
            .snapshot
            .plan
            .as_ref()
            .map_or(ActiveRegion::UNBOUNDED, |p| p.region_at(frame));
        if let Some(plan) = &self.active()?.snapshot.plan {
            let correction = next.correction();
            let mut end = next.end();
            for _ in 0..=plan.segments.len() {
                if end == u64::MAX {
                    break;
                }
                let following = plan.region_at(end);
                if (following.correction() - correction).abs() > Self::RATIO_EPS {
                    break;
                }
                end = following.end();
            }
            next = ActiveRegion::new(next.start(), end, correction);
        }
        let core = self.active_mut()?;
        let crossed = core.region.is_some();
        if !crossed {
            core.region = Some(next);
        }
        Ok((next, crossed))
    }

    pub(super) fn request_region_boundary(&mut self, source_frame: u64) -> DecodeResult<()> {
        match self.region_boundary {
            None => self.region_boundary = Some(RegionBoundary { source_frame }),
            Some(boundary) if boundary.source_frame == source_frame => {}
            Some(_) => {
                return Err(DecodeError::InvalidData {
                    detail: "tempo stage crossed a second region boundary before commit",
                });
            }
        }
        Ok(())
    }

    pub(super) fn begin_tempo_drain(&mut self, context: &'static str) -> DecodeResult<()> {
        if self.eof_pending || self.discontinuity_pending {
            return Err(DecodeError::InvalidData {
                detail: "tempo stage already has an active drain debt",
            });
        }
        let held_source_frames = self.active_held_source_frames();
        let core = self.active_mut()?;
        core.scratch.clear();
        if core.processing {
            let bound = core.backend()?.max_tail_samples();
            if bound > core.scratch.capacity() {
                return Err(DecodeError::InvalidData {
                    detail: "tempo tail exceeds prepared storage",
                });
            }
            let (backend, scratch) = core
                .backend
                .as_deref_mut()
                .zip(Some(&mut core.scratch))
                .ok_or(DecodeError::InvalidData {
                    detail: "active tempo core lost its DSP backend",
                })?;
            backend
                .flush(scratch)
                .map_err(|_| DecodeError::InvalidData { detail: context })?;
            if core.scratch.len() > bound || core.scratch.len() % core.channels() != 0 {
                return Err(DecodeError::InvalidData {
                    detail: "tempo backend violated its tail bound",
                });
            }
        }
        self.eof_offset = 0;
        let core = self.active()?;
        let tail_frames = core.scratch.len() / core.channels();
        let disposition = core
            .backend
            .as_ref()
            .map_or(DrainDisposition::DiscardHeld, |backend| {
                backend.drain_disposition()
            });
        self.drain_frontier = Some(match disposition {
            DrainDisposition::RenderedTail => DrainFrontier::new(held_source_frames, tail_frames)?,
            DrainDisposition::DiscardHeld if tail_frames == 0 => DrainFrontier::new(0, 0)?,
            DrainDisposition::DiscardHeld => {
                return Err(DecodeError::InvalidData {
                    detail: "discarding tempo backend emitted an undeclared drain tail",
                });
            }
            _ => {
                return Err(DecodeError::InvalidData {
                    detail: "tempo backend declared an unsupported drain disposition",
                });
            }
        });
        Ok(())
    }

    pub(super) fn render_tempo_drain(
        &mut self,
        mut credit: OutputCredit<'_>,
    ) -> DecodeResult<Option<(usize, PcmMeta)>> {
        self.validate_credit(&mut credit)?;
        if self.eof_offset >= self.active()?.scratch.len() {
            let core = self.active_mut()?;
            core.scratch.clear();
            core.processing = false;
            core.applied_stretch = f64::NAN;
            core.applied_pitch = f64::NAN;
            core.source_frames_pushed = 0;
            self.eof_offset = 0;
            self.eof_pending = false;
            self.discontinuity_pending = false;
            self.drain_frontier = None;
            return Ok(None);
        }
        let channels = self.channels();
        let available_frames = (self.active()?.scratch.len() - self.eof_offset) / channels;
        let frames = available_frames.min(credit.max_frames());
        let samples = frames
            .checked_mul(channels)
            .ok_or(DecodeError::InvalidData {
                detail: "tempo drain sample count overflow",
            })?;
        let end = self
            .eof_offset
            .checked_add(samples)
            .ok_or(DecodeError::InvalidData {
                detail: "tempo drain sample range overflow",
            })?;
        credit.samples_mut()[..samples]
            .copy_from_slice(&self.active()?.scratch[self.eof_offset..end]);
        self.eof_offset = end;
        let frontier = self
            .drain_frontier
            .as_mut()
            .ok_or(DecodeError::InvalidData {
                detail: "tempo drain lost its source frontier",
            })?;
        frontier.advance(frames)?;
        let spec = self.active()?.spec;
        let mut meta = self.last_input_meta.ok_or(DecodeError::InvalidData {
            detail: "tempo drain has no admitted source metadata",
        })?;
        meta.spec = spec;
        meta.frames = u32::try_from(frames).map_err(|_| DecodeError::InvalidData {
            detail: "tempo drain frame count exceeds u32",
        })?;
        Ok(Some((frames, meta)))
    }
}

impl TempoCore {
    pub(super) fn backend(&self) -> DecodeResult<&dyn StretchBackend> {
        self.backend.as_deref().ok_or(DecodeError::InvalidData {
            detail: "tempo bypass core has no DSP backend",
        })
    }

    pub(super) fn backend_mut(&mut self) -> DecodeResult<&mut (dyn StretchBackend + 'static)> {
        self.backend.as_deref_mut().ok_or(DecodeError::InvalidData {
            detail: "tempo bypass core has no DSP backend",
        })
    }

    pub(super) fn bypass(&self) -> bool {
        self.backend.is_none()
    }

    pub(super) fn channels(&self) -> usize {
        usize::from(self.spec.channels.max(1))
    }
}

impl RetiredCores {
    pub(super) fn release(self) {
        drop(self.active);
        drop(self.prepared);
    }
}

pub(super) struct AdmittedSource {
    pub(super) chunk: PcmChunk,
    pub(super) consumed_frames: usize,
}

impl AdmittedSource {
    pub(super) fn sample_range(
        &self,
        frames: usize,
        channels: usize,
    ) -> DecodeResult<Range<usize>> {
        let start = self
            .consumed_frames
            .checked_mul(channels)
            .ok_or(DecodeError::InvalidData {
                detail: "tempo source sample offset overflow",
            })?;
        let samples = frames
            .checked_mul(channels)
            .ok_or(DecodeError::InvalidData {
                detail: "tempo source sample count overflow",
            })?;
        let end = start.checked_add(samples).ok_or(DecodeError::InvalidData {
            detail: "tempo source sample range overflow",
        })?;
        Ok(start..end)
    }
}

pub(super) struct DrainFrontier {
    emitted_output_frames: usize,
    pub(super) remaining_source_frames: u64,
    total_output_frames: usize,
    total_source_frames: u64,
}

impl DrainFrontier {
    pub(super) fn new(total_source_frames: u64, total_output_frames: usize) -> DecodeResult<Self> {
        if total_source_frames != 0 && total_output_frames == 0 {
            return Err(DecodeError::InvalidData {
                detail: "tempo backend retained source without a rendered drain tail",
            });
        }
        Ok(Self {
            emitted_output_frames: 0,
            remaining_source_frames: total_source_frames,
            total_output_frames,
            total_source_frames,
        })
    }

    pub(super) fn advance(&mut self, output_frames: usize) -> DecodeResult<()> {
        self.emitted_output_frames = self
            .emitted_output_frames
            .checked_add(output_frames)
            .filter(|emitted| *emitted <= self.total_output_frames)
            .ok_or(DecodeError::InvalidData {
                detail: "tempo drain output exceeded its declared tail",
            })?;
        if self.total_output_frames == 0 {
            self.remaining_source_frames = 0;
            return Ok(());
        }
        let emitted =
            u128::try_from(self.emitted_output_frames).map_err(|_| DecodeError::InvalidData {
                detail: "tempo drain output progress exceeds u128",
            })?;
        let total_output =
            u128::try_from(self.total_output_frames).map_err(|_| DecodeError::InvalidData {
                detail: "tempo drain output length exceeds u128",
            })?;
        let released = u128::from(self.total_source_frames)
            .checked_mul(emitted)
            .ok_or(DecodeError::InvalidData {
                detail: "tempo drain source allocation overflow",
            })?
            / total_output;
        let released = u64::try_from(released).map_err(|_| DecodeError::InvalidData {
            detail: "tempo drain released source exceeds u64",
        })?;
        self.remaining_source_frames =
            self.total_source_frames
                .checked_sub(released)
                .ok_or(DecodeError::InvalidData {
                    detail: "tempo drain released more source than it retained",
                })?;
        Ok(())
    }
}

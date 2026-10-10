use std::mem;

use kithara_bufpool::HasPool;
use kithara_signal::SourceSpan;
use kithara_stretch::ElasticError;
use num_traits::ToPrimitive;

use super::{super::trajectory::Trajectory, core::WarpRenderer, target::PreparedTarget};

pub(in crate::render) struct RetiringTarget {
    pub(in crate::render) trajectory: Option<Trajectory>,
    target: PreparedTarget,
    keylock: bool,
    active: bool,
    pitch: f64,
    frames: usize,
    rendered: usize,
}

impl RetiringTarget {
    pub(in crate::render) fn lookahead(&self, stages: usize) -> Result<usize, ElasticError> {
        if !self.keylock {
            return Ok(0);
        }
        let latency = self
            .target
            .engine
            .as_ref()
            .ok_or(ElasticError::EnginePreparation("engine is unavailable"))?
            .capabilities()
            .latency();
        latency
            .first()
            .checked_add(latency.second())
            .and_then(|frames| frames.checked_mul(stages))
            .ok_or(ElasticError::SampleCountOverflow)
    }

    pub(in crate::render) fn complete(&self) -> bool {
        self.rendered == self.frames
    }

    pub(in crate::render) fn extend(&mut self, latency: usize) {
        self.frames = self.frames.max(latency);
    }
}

impl<S: HasPool<f32>> WarpRenderer<S> {
    pub(in crate::render) fn projection_lookahead(&self) -> Result<usize, ElasticError> {
        let stages = self.projection_stages()?;
        let latency = self
            .engine
            .as_ref()
            .ok_or(ElasticError::EnginePreparation("engine is unavailable"))?
            .capabilities()
            .latency();
        let frames = latency
            .first()
            .checked_add(latency.second())
            .and_then(|frames| frames.checked_mul(stages))
            .ok_or(ElasticError::SampleCountOverflow)?;
        self.retiring_target.as_ref().map_or(Ok(frames), |target| {
            target.lookahead(stages).map(|old| old.max(frames))
        })
    }

    pub(in crate::render) fn retire_mapped_target(&mut self) -> Result<(), ElasticError> {
        let mut frames = self
            .engine_latency()
            .get()
            .clamp(1, Self::MAX_OUTPUT_FRAMES);
        if self.engine.is_none() {
            return Err(ElasticError::EnginePreparation("engine is unavailable"));
        }
        let unity = self.stretch_target().1 && self.keylocked_unity();
        let trajectory = unity.then(|| self.trajectory.clone());
        if unity {
            let mut identity = self.trajectory.clone();
            identity.snap_to_frame()?;
            let start = identity.span(0, self.spec.sample_rate, 1)?.start();
            if let Some(end) = self.residency.as_ref().and_then(|resident| resident.end) {
                frames = frames.max(
                    usize::try_from(end.saturating_sub(start))
                        .map_err(|_| ElasticError::SampleCountOverflow)?,
                );
            }
            self.trajectory = identity;
        }
        self.retiring_target = Some(RetiringTarget {
            trajectory,
            target: PreparedTarget {
                engine: self.engine.take(),
                projection: self.projection.take(),
                activation_scratch: self.activation_scratch.take(),
                pending_source: self.pending_source.take(),
                scratch: self.scratch.take(),
                residency: None,
            },
            keylock: self.current_keylock,
            active: unity && self.active,
            pitch: f64::NAN,
            frames,
            rendered: 0,
        });
        Ok(())
    }

    fn swap_target(&mut self, target: &mut PreparedTarget) {
        mem::swap(&mut self.engine, &mut target.engine);
        mem::swap(&mut self.projection, &mut target.projection);
        mem::swap(&mut self.activation_scratch, &mut target.activation_scratch);
        mem::swap(&mut self.pending_source, &mut target.pending_source);
        mem::swap(&mut self.scratch, &mut target.scratch);
    }

    fn render_mapping(&mut self, span: SourceSpan) -> Result<(), ElasticError> {
        if self.current_keylock {
            self.render_projected(span)?;
        } else {
            self.render_varispeed(span)?;
        }
        self.active = true;
        Ok(())
    }

    pub(in crate::render) fn render_mapped(
        &mut self,
        span: SourceSpan,
    ) -> Result<(), ElasticError> {
        if self.keylocked_unity() {
            self.render_varispeed(span)?;
            self.active = false;
        } else {
            self.render_mapping(span)?;
        }
        let Some(mut retiring) = self.retiring_target.take() else {
            return Ok(());
        };
        let result = self.blend_retiring(span, &mut retiring);
        self.retiring_target = Some(retiring);
        result
    }

    fn blend_retiring(
        &mut self,
        span: SourceSpan,
        retiring: &mut RetiringTarget,
    ) -> Result<(), ElasticError> {
        let frames = usize::try_from(span.output_frames())
            .map_err(|_| ElasticError::SampleCountOverflow)?
            .min(retiring.frames - retiring.rendered);
        if frames == 0 {
            return Ok(());
        }
        let span = span
            .for_output_range(
                0..u64::try_from(frames).map_err(|_| ElasticError::SampleCountOverflow)?,
            )
            .ok_or(ElasticError::SampleCountOverflow)?;
        let tail_span = retiring
            .trajectory
            .as_ref()
            .map(|trajectory| trajectory.span(span.start(), self.spec.sample_rate, frames))
            .transpose()?;
        self.swap_target(&mut retiring.target);
        let keylock = self.current_keylock;
        self.current_keylock = retiring.keylock && (keylock || retiring.trajectory.is_some());
        mem::swap(&mut self.active, &mut retiring.active);
        mem::swap(&mut self.applied_pitch, &mut retiring.pitch);
        let result = if let (Some(trajectory), Some(tail_span)) =
            (retiring.trajectory.as_mut(), tail_span)
        {
            if let Some(scratch) = self.scratch.as_mut() {
                scratch.clear();
            }
            self.drain_tail(usize::from(self.spec.channels), frames)
                .and_then(|complete| {
                    self.active = !complete;
                    trajectory.advance(tail_span)
                })
        } else {
            self.render_mapping(span)
        };
        mem::swap(&mut self.applied_pitch, &mut retiring.pitch);
        mem::swap(&mut self.active, &mut retiring.active);
        self.current_keylock = keylock;
        self.swap_target(&mut retiring.target);
        result?;
        let channels = usize::from(self.spec.channels);
        let old = retiring
            .target
            .scratch
            .as_ref()
            .ok_or(ElasticError::PoolCapacity)?;
        let output = self.scratch.as_mut().ok_or(ElasticError::PoolCapacity)?;
        for (frame, (old, output)) in old
            .chunks_exact(channels)
            .zip(output.chunks_exact_mut(channels))
            .take(frames)
            .enumerate()
        {
            let mix = (retiring.rendered + frame + 1)
                .to_f32()
                .ok_or(ElasticError::SampleCountOverflow)?
                / retiring
                    .frames
                    .to_f32()
                    .ok_or(ElasticError::SampleCountOverflow)?;
            for (old, output) in old.iter().zip(output) {
                if old != output {
                    *output = old.mul_add(1.0 - mix, *output * mix);
                }
            }
        }
        retiring.rendered += frames;
        Ok(())
    }
}

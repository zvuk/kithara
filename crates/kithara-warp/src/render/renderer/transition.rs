use std::{mem, num::NonZeroUsize};

use kithara_bufpool::HasPool;
use kithara_dsp::crossfade_linear;
use kithara_signal::SourceSpan;
use kithara_stretch::{ElasticCapabilities, ElasticError};
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
    fn blend_output(
        &mut self,
        output: &mut [f32],
        frames: usize,
        channels: usize,
    ) -> Result<(), ElasticError> {
        let old = self
            .target
            .scratch
            .as_ref()
            .ok_or(ElasticError::PoolCapacity)?;
        let channels = NonZeroUsize::new(channels).ok_or(ElasticError::SampleCountOverflow)?;
        let samples = old.len().min(frames.saturating_mul(channels.get()));
        crossfade_linear(
            &old[..samples],
            output,
            channels,
            self.rendered + 1,
            self.frames,
        );
        self.rendered += frames;
        Ok(())
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
        let capabilities = self
            .engine
            .as_ref()
            .ok_or(ElasticError::EnginePreparation("engine is unavailable"))?
            .capabilities();
        let frames = self.target_lookahead(capabilities, self.current_keylock)?;
        self.retiring_target.as_ref().map_or(Ok(frames), |target| {
            let capabilities = target
                .target
                .engine
                .as_ref()
                .ok_or(ElasticError::EnginePreparation("engine is unavailable"))?
                .capabilities();
            self.target_lookahead(capabilities, target.keylock && target.trajectory.is_none())
                .map(|old| old.max(frames))
        })
    }

    fn target_lookahead(
        &self,
        capabilities: ElasticCapabilities,
        keylock: bool,
    ) -> Result<usize, ElasticError> {
        if !keylock {
            return Ok(0);
        }
        let latency = capabilities.latency();
        if let Some(speed) = self.native_projection_speed(capabilities, keylock)? {
            (latency
                .first()
                .to_f64()
                .ok_or(ElasticError::SampleCountOverflow)?
                / speed
                + self
                    .native_projection_packet(speed, capabilities)?
                    .to_f64()
                    .ok_or(ElasticError::SampleCountOverflow)?)
            .ceil()
            .to_usize()
            .and_then(|history| history.checked_add(latency.second()))
            .ok_or(ElasticError::SampleCountOverflow)
        } else {
            let stages = self.projection_stages()?;
            latency
                .first()
                .checked_add(latency.second())
                .and_then(|frames| frames.checked_mul(stages))
                .ok_or(ElasticError::SampleCountOverflow)
        }
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
        let output = self.scratch.as_mut().ok_or(ElasticError::PoolCapacity)?;
        retiring.blend_output(output, frames, channels)
    }
}

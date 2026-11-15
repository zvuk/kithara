use std::mem;

use kithara_bufpool::HasPool;
use kithara_signal::SourceSpan;
use kithara_stretch::ElasticError;
use kithara_test_macros as kithara;
use num_traits::ToPrimitive;

use super::core::WarpRenderer;

impl<S: HasPool<f32>> WarpRenderer<S> {
    pub(in crate::render) fn projection_lookahead(&self) -> Result<usize, ElasticError> {
        if !self.current_pitch_engine {
            return Ok(0);
        }
        let capabilities = self
            .engine
            .as_ref()
            .ok_or(ElasticError::EnginePreparation("engine is unavailable"))?
            .capabilities();
        let latency = capabilities.latency();
        if let Some((speed, maximum)) =
            self.native_projection_rates(capabilities, self.pitch_controls().keylock())?
        {
            (latency
                .first()
                .to_f64()
                .ok_or(ElasticError::SampleCountOverflow)?
                / speed
                + self
                    .native_projection_packet(maximum, capabilities)?
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

    fn render_mapping(&mut self, span: SourceSpan) -> Result<(), ElasticError> {
        if self.current_pitch_engine {
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
        if let Some(retired) = self.stateless_tail_owed.take() {
            let law = mem::replace(&mut self.trajectory, retired);
            let captured = self.capture_stateless_tail(span.start());
            debug_assert!(self.deferred_trajectory.is_none());
            self.deferred_trajectory = Some(mem::replace(&mut self.trajectory, law));
            captured?;
        }
        if self.keylocked_unity() {
            self.render_varispeed(span)?;
            self.active = false;
        } else {
            self.render_mapping(span)?;
        }
        Ok(())
    }

    /// Sample the retired stateless side from `start` on under its captured law
    /// into the replacement, as far as the residency and its capacity hold it.
    #[kithara::hang_watchdog]
    fn capture_stateless_tail(&mut self, start: u64) -> Result<(), ElasticError> {
        let channels = usize::from(self.spec.channels.max(1));
        let reach = self.source_reach()?;
        let (resident, scratch) = self
            .residency
            .as_mut()
            .zip(self.scratch.as_ref())
            .ok_or(ElasticError::PoolCapacity)?;
        resident.next_replacement.clear();
        let mut remaining = resident.next_replacement.capacity() / channels;
        let piece = scratch.capacity() / channels;
        let limit = resident
            .end
            .ok_or(ElasticError::EmptySource)?
            .saturating_sub(reach);
        while remaining > 0 {
            hang_tick!();
            let Some(folded) =
                self.capture_stateless_piece(start, remaining.min(piece), limit, channels)?
            else {
                break;
            };
            remaining -= folded;
            hang_reset!();
        }
        self.residency
            .as_mut()
            .ok_or(ElasticError::PoolCapacity)?
            .finish_fold();
        Ok(())
    }

    /// Fold the next piece of the stateless tail, at most `frames` long, and
    /// return the frames folded. Returns `None` once the tail reaches `limit`,
    /// after folding the part of the piece before it.
    fn capture_stateless_piece(
        &mut self,
        start: u64,
        frames: usize,
        limit: u64,
        channels: usize,
    ) -> Result<Option<usize>, ElasticError> {
        let frames = self.trajectory.output_limit(frames);
        let span = self.mapping_span(start, self.spec.sample_rate, frames)?;
        let Some(sampled) = Self::span_until(span, limit)? else {
            return Ok(None);
        };
        self.render_varispeed(sampled)?;
        let (resident, output) = self
            .residency
            .as_mut()
            .zip(self.scratch.as_mut())
            .ok_or(ElasticError::PoolCapacity)?;
        let folded = resident.fold_supply(output, channels)?;
        output.clear();
        self.trajectory.advance(sampled)?;
        Ok((sampled.output_frames() >= span.output_frames()).then_some(folded))
    }
}

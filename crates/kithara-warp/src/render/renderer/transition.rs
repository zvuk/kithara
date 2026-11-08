use std::mem;

use kithara_bufpool::HasPool;
use kithara_signal::SourceSpan;
use kithara_stretch::ElasticError;
use kithara_test_macros as kithara;
use num_traits::ToPrimitive;

use super::core::WarpRenderer;

impl<S: HasPool<f32>> WarpRenderer<S> {
    pub(in crate::render) fn projection_lookahead(&self) -> Result<usize, ElasticError> {
        if !self.current_keylock {
            return Ok(0);
        }
        let capabilities = self
            .engine
            .as_ref()
            .ok_or(ElasticError::EnginePreparation("engine is unavailable"))?
            .capabilities();
        let latency = capabilities.latency();
        if let Some(speed) = self.native_projection_speed(capabilities, self.current_keylock)? {
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
        if mem::take(&mut self.stateless_tail_owed) {
            let law = self.trajectory.clone();
            let captured = self.capture_stateless_tail(span.start());
            self.trajectory = law;
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

    /// Sample the retired stateless side from `start` on under the current law
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
            let frames = self.trajectory.output_limit(remaining.min(piece));
            let span = self.mapping_span(start, self.spec.sample_rate, frames)?;
            let Some(sampled) = Self::span_until(span, limit)? else {
                break;
            };
            self.render_varispeed(sampled)?;
            let (resident, output) = self
                .residency
                .as_mut()
                .zip(self.scratch.as_mut())
                .ok_or(ElasticError::PoolCapacity)?;
            remaining -= resident.fold_supply(output, channels)?;
            output.clear();
            self.advance_trajectory(sampled)?;
            hang_reset!();
            if sampled.output_frames() < span.output_frames() {
                break;
            }
        }
        self.residency
            .as_mut()
            .ok_or(ElasticError::PoolCapacity)?
            .finish_fold();
        Ok(())
    }
}

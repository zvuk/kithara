use std::ops::Range;

use kithara_bufpool::HasPool;
use kithara_platform::sync::Arc;
use kithara_signal::SourceSpan;
use kithara_stretch::{ElasticCapabilities, ElasticError, ElasticRequest};
use kithara_sync::SpeedCurve;
use num_traits::ToPrimitive;

use super::{
    super::trajectory::Trajectory,
    core::WarpRenderer,
    mapping::{fraction, span_speed},
    projected::Projection,
};
use crate::WarpRenderError;

pub(super) struct NativePacket {
    pub(super) source: Range<u64>,
    pub(super) request: ElasticRequest,
    pub(super) audible: u64,
}

impl<S: HasPool<f32>> WarpRenderer<S> {
    /// Takes the speed step `curve` opens with into the law `lead` output frames
    /// before [`Self::set_speed`] applies it, and returns whether the law changed.
    ///
    /// A live native keylock engine reads its law one latency ahead: it renders
    /// through a step known that early, and `set_speed` then keeps the stream.
    /// The step is taken when `lead` is at least that latency, the law holds
    /// its speed, and `curve` opens on another speed the engine renders
    /// natively, unity excepted. Any other step re-primes the engine on its
    /// frame. A later call moves the step; [`Self::reset`] withdraws it.
    ///
    /// # Errors
    /// Returns an error when the stepped law is not representable.
    pub fn anticipate_speed(
        &mut self,
        curve: &SpeedCurve,
        lead: u64,
    ) -> Result<bool, WarpRenderError> {
        let Some(speed) = self.step_ahead(curve, lead)? else {
            return Ok(false);
        };
        let held = self.trajectory.speed()?;
        let mut ahead = self.trajectory.clone();
        ahead.replace(SpeedCurve::Steps(Arc::from([(lead, speed)])))?;
        if !self.can_continue_native(&ahead)? {
            return Ok(false);
        }
        self.speed_ahead = Some(held);
        self.trajectory = ahead;
        self.prepared_quantum = None;
        Ok(true)
    }

    /// Whether the engine can still read a step `lead` output frames ahead
    /// that its law does not hold yet.
    fn reads_ahead(&self, lead: u64) -> Result<bool, ElasticError> {
        let latency = u64::try_from(self.engine_latency().get())
            .map_err(|_| ElasticError::SampleCountOverflow)?;
        Ok(self.native_stream_live()
            && lead >= latency
            && (self.speed_ahead.is_some() || self.trajectory.holds_speed())
            && self.trajectory.frames_to_step() != Some(lead))
    }

    /// The speed the law steps to `lead` output frames ahead for `curve`, or
    /// `None` when that step is left to its frame.
    fn step_ahead(&self, curve: &SpeedCurve, lead: u64) -> Result<Option<f32>, ElasticError> {
        if !self.reads_ahead(lead)? {
            return Ok(None);
        }
        let mut next = self.trajectory.clone();
        let Ok(speed) = next.replace(curve.clone()) else {
            return Ok(None);
        };
        let held = self.trajectory.speed()?;
        let capabilities = self
            .engine
            .as_ref()
            .ok_or(ElasticError::PoolCapacity)?
            .capabilities();
        let steps = speed.to_bits() != held.to_bits() && speed.to_bits() != 1.0_f32.to_bits();
        Ok((steps && self.native_rates_at(&next, capabilities, true)?.is_some()).then_some(speed))
    }

    pub(in crate::render) fn can_continue_native(
        &self,
        trajectory: &Trajectory,
    ) -> Result<bool, ElasticError> {
        if !self.native_stream_live() || trajectory.constant_unity() {
            return Ok(false);
        }
        if !self.native_velocity_matches(trajectory)? {
            return Ok(false);
        }
        let Some((start, cached)) = self.native_cached_origin(trajectory)? else {
            return Ok(false);
        };
        let capabilities = self
            .engine
            .as_ref()
            .ok_or(ElasticError::PoolCapacity)?
            .capabilities();
        let Some((_, maximum)) = self.native_rates_at(trajectory, capabilities, true)? else {
            return Ok(false);
        };
        let maximum = self.native_projection_packet(maximum, capabilities)?;
        Ok(self
            .native_packet_plan(trajectory, start, cached, maximum)?
            .is_some())
    }

    fn native_cached_origin(
        &self,
        trajectory: &Trajectory,
    ) -> Result<Option<(u64, usize)>, ElasticError> {
        let Some(projection) = self
            .projection
            .as_ref()
            .filter(|projection| projection.feed.is_some() && projection.audible.is_some())
        else {
            return Ok(None);
        };
        let Some((start, rate)) = self.rendered_source_end else {
            return Ok(None);
        };
        if rate != self.spec.sample_rate
            || !self.native_cache_matches(projection, trajectory, start)?
        {
            return Ok(None);
        }
        Ok(Some((start, self.native_cached_frames(projection)?)))
    }

    fn native_stream_live(&self) -> bool {
        self.active
            && self.mapped_render
            && self.pitch_controls().keylock()
            && self.stretch_target() == (self.current_kind, self.current_pitch_engine)
            && self.terminal_source_end.is_none()
            && self.pending_unity_meta.is_none()
            && !self.backend_transition_pending
            && !self.reset_pending
            && !self.rebuild_pending
    }

    fn native_velocity_matches(&self, trajectory: &Trajectory) -> Result<bool, ElasticError> {
        let speed = trajectory.speed()?.to_bits();
        if speed == self.trajectory.speed()?.to_bits() {
            return Ok(true);
        }
        if !self.unity_retirement_pending() {
            return Ok(false);
        }
        let Some(span) = self
            .projection
            .as_ref()
            .and_then(|projection| projection.cached_span)
            .map(|(_, span)| span)
            .filter(|span| span.is_affine())
        else {
            return Ok(false);
        };
        let held = span_speed(span, 0)?
            .to_f32()
            .ok_or(ElasticError::SampleCountOverflow)?;
        Ok(held.to_bits() == speed)
    }

    fn native_cached_frames(&self, projection: &Projection) -> Result<usize, ElasticError> {
        projection.buffers[0]
            .len()
            .checked_sub(projection.offset)
            .and_then(|samples| samples.checked_div(usize::from(self.spec.channels)))
            .ok_or(ElasticError::SampleCountOverflow)
    }

    pub(super) fn native_packet_plan(
        &self,
        trajectory: &Trajectory,
        start: u64,
        output: usize,
        maximum: usize,
    ) -> Result<Option<NativePacket>, ElasticError> {
        let projection = self.projection.as_ref().ok_or(ElasticError::PoolCapacity)?;
        let input = projection.feed.ok_or(ElasticError::EmptySource)?;
        let audible = projection.audible.ok_or(ElasticError::EmptySource)?;
        let capabilities = self
            .engine
            .as_ref()
            .ok_or(ElasticError::PoolCapacity)?
            .capabilities();
        let preferred = crate::consts::NATIVE_CONTROL_PACKET_FRAMES.min(maximum);
        for frames in (preferred..maximum)
            .step_by(crate::consts::NATIVE_CONTROL_PACKET_FRAMES)
            .chain([maximum])
        {
            let boundary = output
                .checked_add(frames)
                .ok_or(ElasticError::SampleCountOverflow)?;
            let end = self.native_boundary_at(trajectory, start, boundary)?;
            let heard = self.native_source_boundary(trajectory, start, boundary)?;
            if let Some(request) =
                self.native_span_request(input..end, audible..heard, frames, capabilities)?
            {
                return Ok(Some(NativePacket {
                    source: input..end,
                    request,
                    audible: heard,
                }));
            }
        }
        Ok(None)
    }

    fn native_span_request(
        &self,
        source: Range<u64>,
        audible: Range<u64>,
        output: usize,
        capabilities: ElasticCapabilities,
    ) -> Result<Option<ElasticRequest>, ElasticError> {
        let Some(source_frames) = source
            .end
            .checked_sub(source.start)
            .filter(|frames| *frames > 0)
        else {
            return Ok(None);
        };
        let Some(audible_frames) = audible
            .end
            .checked_sub(audible.start)
            .filter(|frames| *frames > 0)
        else {
            return Ok(None);
        };
        let source_frames =
            usize::try_from(source_frames).map_err(|_| ElasticError::SampleCountOverflow)?;
        let audible_frames =
            usize::try_from(audible_frames).map_err(|_| ElasticError::SampleCountOverflow)?;
        let request = ElasticRequest::new(source_frames, output)?
            .with_output_source_frames(audible_frames)?;
        Ok(self
            .native_request_fits(request, capabilities)?
            .then_some(request))
    }

    fn native_request_fits(
        &self,
        request: ElasticRequest,
        capabilities: ElasticCapabilities,
    ) -> Result<bool, ElasticError> {
        let output = request
            .output_frames()
            .to_f64()
            .ok_or(ElasticError::SampleCountOverflow)?;
        let input = request
            .source_frames()
            .to_f64()
            .ok_or(ElasticError::SampleCountOverflow)?;
        let audible = request
            .output_source_frames()
            .to_f64()
            .ok_or(ElasticError::SampleCountOverflow)?;
        let envelope = capabilities.rate_envelope();
        Ok(request.source_frames()
            <= self
                .source_block_frames
                .get()
                .min(capabilities.max_source_frames())
            && request.output_frames() <= capabilities.max_output_frames()
            && envelope.contains_rate(input / output)
            && envelope.contains_rate(audible / output))
    }

    /// Geometry of the packet of `frames` outputs that starts `output` frames
    /// past `start`: its span from the last boundary of the law inside it on,
    /// and the packet frame that span starts at.
    pub(super) fn native_cached_span(
        &self,
        start: u64,
        output: usize,
        frames: usize,
    ) -> Result<(usize, SourceSpan), ElasticError> {
        let mut cursor = self.trajectory.clone();
        cursor.advance_by(
            start,
            self.spec.sample_rate,
            u64::try_from(output).map_err(|_| ElasticError::SampleCountOverflow)?,
        )?;
        let mut from = 0;
        let mut piece = cursor.output_limit(frames);
        while piece < frames - from {
            cursor.advance_by(
                start,
                self.spec.sample_rate,
                u64::try_from(piece).map_err(|_| ElasticError::SampleCountOverflow)?,
            )?;
            from += piece;
            piece = cursor.output_limit(frames - from);
        }
        Ok((from, cursor.span(start, self.spec.sample_rate, piece)?))
    }

    fn native_cache_matches(
        &self,
        projection: &Projection,
        trajectory: &Trajectory,
        start: u64,
    ) -> Result<bool, ElasticError> {
        let cached = self.native_cached_frames(projection)?;
        if cached == 0 {
            return Ok(true);
        }
        let Some((from, span)) = projection.cached_span else {
            return Ok(false);
        };
        let Some(offset) = projection
            .offset
            .checked_div(usize::from(self.spec.channels))
            .ok_or(ElasticError::SampleCountOverflow)?
            .checked_sub(from)
        else {
            return Ok(false);
        };
        for frame in 0..=cached {
            let previous = offset
                .checked_add(frame)
                .and_then(|frame| u64::try_from(frame).ok())
                .and_then(|frame| span.source_ratio_at(frame))
                .map(fraction)
                .ok_or(ElasticError::SampleCountOverflow)?;
            let current = fraction(trajectory.at_offset(
                start,
                self.spec.sample_rate,
                u64::try_from(frame).map_err(|_| ElasticError::SampleCountOverflow)?,
            )?);
            let difference = previous
                .sub(current)
                .or_else(|| current.sub(previous))
                .ok_or(ElasticError::SampleCountOverflow)?;
            if difference.numerator > difference.denominator.get() {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

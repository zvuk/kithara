use std::io::Error as IoError;

use kithara_decode::PcmChunk;
use kithara_stream::{ChunkPosition, PlayheadWrite};
use num_traits::cast::ToPrimitive;

use super::{
    ChunkCursor, NANOS_PER_SECOND_F64, VarispeedReadiness, chunk_span_ns, frames_to_samples,
    interpolated_fractional_position,
};
use crate::{
    audio::{
        ConsumerPhase, DecodeError,
        event::AudioEvents,
        ring::{Lookahead, RecvCtx, RingConsumer},
    },
    effects::transport::{MAX_AUDIBLE_RATE, MIN_PITCH_BEND},
};

impl ChunkCursor {
    pub(super) fn source_cursor_at_or_past(&self, total_frames: u64) -> bool {
        self.source_cursor >= total_frames.to_f64().unwrap_or(f64::INFINITY)
    }

    pub(super) fn residual_after(&self, total_frames: u64) -> f64 {
        let total = total_frames.to_f64().unwrap_or(f64::INFINITY);
        (self.source_cursor - total).max(0.0)
    }

    pub(super) fn varispeed_readiness(
        &mut self,
        ring: &mut RingConsumer,
        events: &mut AudioEvents,
        playhead: &dyn PlayheadWrite,
        recv: RecvCtx<'_>,
        total_frames: u64,
        multiplier: f64,
    ) -> VarispeedReadiness {
        let sample_needs_next = self.sample_needs_next(total_frames);
        let total = total_frames.to_f64().unwrap_or(f64::INFINITY);
        let advance_needs_next = self.source_cursor + multiplier > total;
        if !sample_needs_next && !advance_needs_next {
            return VarispeedReadiness::Ready;
        }

        let had_lookahead = ring.lookahead.is_some();
        let was_playing = ring.phase == ConsumerPhase::Playing;
        let lookahead = ring.ensure_lookahead(recv);
        if !had_lookahead {
            events.fill_result(
                matches!(lookahead, Lookahead::Available),
                was_playing,
                ring.phase.is_terminal(),
                playhead.position(),
                ring.validator.epoch,
            );
        }
        match lookahead {
            Lookahead::Pending => VarispeedReadiness::Pending,
            Lookahead::Eof if sample_needs_next => VarispeedReadiness::FinishCurrent,
            Lookahead::Available | Lookahead::Eof => VarispeedReadiness::Ready,
        }
    }

    fn sample_needs_next(&self, total_frames: u64) -> bool {
        let base_frame = self.source_cursor.floor().to_u64().unwrap_or(u64::MAX);
        let base = base_frame.to_f64().unwrap_or(f64::INFINITY);
        let fraction = self.source_cursor - base;
        fraction > 0.0 && base_frame.saturating_add(1) >= total_frames
    }

    pub(super) fn write_varispeed_frame(
        &self,
        ring: &RingConsumer,
        output: &mut [f32],
        channels: u64,
    ) -> Result<(), DecodeError> {
        let Some(chunk) = ring.current_chunk.as_ref() else {
            return Ok(());
        };

        let channel_samples = frames_to_samples(1, channels)?;
        let base_frame = self.source_cursor.floor().to_u64().unwrap_or(u64::MAX);
        let base = base_frame.to_f64().unwrap_or(f64::INFINITY);
        let fraction = self.source_cursor - base;
        let base_sample = frames_to_samples(base_frame, channels)?;

        if fraction == 0.0 {
            output[..channel_samples]
                .copy_from_slice(&chunk.samples[base_sample..base_sample + channel_samples]);
            return Ok(());
        }

        let next_frame = base_frame.saturating_add(1);
        let next_sample = frames_to_samples(next_frame, channels)?;
        let fraction = fraction.to_f32().unwrap_or(0.0);
        let total_frames = u64::from(chunk.meta.frames);
        for (channel, sample) in output.iter_mut().enumerate().take(channel_samples) {
            let current = chunk.samples[base_sample + channel];
            let next = if next_frame < total_frames {
                chunk.samples[next_sample + channel]
            } else {
                ring.lookahead
                    .as_ref()
                    .and_then(|lookahead| lookahead.samples.get(channel))
                    .copied()
                    .ok_or_else(|| DecodeError::Io {
                        source: IoError::other("missing varispeed seam lookahead"),
                    })?
            };
            *sample = current + (next - current) * fraction;
        }
        Ok(())
    }

    pub(super) fn consume_finished_chunks(
        &mut self,
        ring: &mut RingConsumer,
        playhead: &dyn PlayheadWrite,
    ) {
        loop {
            let Some(chunk) = ring.current_chunk.as_ref() else {
                self.clear();
                break;
            };
            let total_frames = u64::from(chunk.meta.frames);
            let total = total_frames.to_f64().unwrap_or(f64::INFINITY);
            if self.source_cursor < total {
                break;
            }

            let residual = (self.source_cursor - total).max(0.0);
            self.finish_current(ring, playhead, residual);
            if ring.current_chunk.is_none() {
                break;
            }
        }
    }

    pub(super) fn finish_current(
        &mut self,
        ring: &mut RingConsumer,
        playhead: &dyn PlayheadWrite,
        residual: f64,
    ) {
        if let Some(chunk) = ring.current_chunk.as_ref() {
            playhead.advance(&ChunkPosition::from(&chunk.meta));
        }
        ring.recycle_current();

        if let Some(next) = ring.lookahead.take() {
            self.begin_chunk_at(&next, residual);
            ring.current_chunk = Some(next);
            ring.promote_playing();
        } else {
            self.clear();
        }
    }

    pub(super) fn commit_varispeed_position(
        &mut self,
        ring: &RingConsumer,
        playhead: &dyn PlayheadWrite,
    ) {
        let Some(meta) = ring.current_chunk.as_ref().map(|chunk| chunk.meta) else {
            return;
        };
        if self.source_cursor <= 0.0 {
            return;
        }
        self.sync_consumed_frames(u64::from(meta.frames));
        playhead.advance_partial(interpolated_fractional_position(meta, self.source_cursor));
    }

    pub(super) fn effective_multiplier(requested: f32, chunk: &PcmChunk) -> f64 {
        let multiplier = requested
            .clamp(MIN_PITCH_BEND, MAX_AUDIBLE_RATE)
            .min(Self::max_multiplier_for_chunk(chunk))
            .max(MIN_PITCH_BEND);
        f64::from(multiplier)
    }

    fn max_multiplier_for_chunk(chunk: &PcmChunk) -> f32 {
        let tape_speed = Self::chunk_implied_tape_speed(chunk);
        if tape_speed.is_finite() && tape_speed > 0.0 {
            (MAX_AUDIBLE_RATE / tape_speed).clamp(MIN_PITCH_BEND, MAX_AUDIBLE_RATE)
        } else {
            MAX_AUDIBLE_RATE
        }
    }

    fn chunk_implied_tape_speed(chunk: &PcmChunk) -> f32 {
        let span_ns = chunk_span_ns(chunk.meta);
        if span_ns == 0 || chunk.meta.frames == 0 {
            return 1.0;
        }

        let source_seconds = span_ns.to_f64().unwrap_or(f64::INFINITY) / NANOS_PER_SECOND_F64;
        let output_seconds =
            f64::from(chunk.meta.frames) / f64::from(chunk.meta.spec.sample_rate.get());
        (source_seconds / output_seconds)
            .to_f32()
            .unwrap_or(MAX_AUDIBLE_RATE)
    }
}

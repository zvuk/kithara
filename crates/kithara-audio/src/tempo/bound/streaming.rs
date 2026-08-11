use kithara_decode::{DecodeError, DecodeResult, PcmChunk};
use kithara_platform::sync::Arc;
use kithara_stretch::{ElasticPriming, StretchBackend};
use tracing::warn;

use super::BoundRenderer;
use crate::{region::ActiveRegion, tempo::streaming::StretchControls};

impl<E: ElasticPriming> BoundRenderer<E> {
    const MIN_SPEED: f32 = 0.05;
    const RATIO_EPS: f64 = 1.0e-4;

    pub(crate) fn flush_streaming(&mut self) -> Option<PcmChunk>
    where
        E: StretchBackend,
    {
        if !self.streaming_active {
            return None;
        }
        self.scratch.clear();
        if let Err(error) = StretchBackend::flush(&mut self.engine, &mut self.scratch) {
            warn!(%error, "resident tempo engine flush failed");
            return None;
        }
        self.emit()
    }

    pub(crate) fn held_streaming_source_frames(&self) -> u64
    where
        E: StretchBackend,
    {
        if !self.streaming_active {
            return 0;
        }
        u64::try_from(self.engine.source_latency_frames())
            .unwrap_or(u64::MAX)
            .min(self.streaming_source_frames)
    }

    pub(crate) fn process_streaming(
        &mut self,
        chunk: PcmChunk,
        controls: &StretchControls,
    ) -> DecodeResult<Option<PcmChunk>>
    where
        E: StretchBackend,
    {
        self.sync_streaming_plan(controls);
        let speed = controls.speed().max(Self::MIN_SPEED);
        if self.streaming_plan.is_none() && (speed - 1.0).abs() <= f32::EPSILON {
            self.reset_streaming_passthrough();
            return Ok(Some(chunk));
        }

        if chunk.spec() != self.spec {
            self.spec = chunk.spec();
        }
        self.streaming_active = true;
        self.last_input_meta = Some(chunk.meta);
        self.scratch.clear();

        let base = 1.0 / f64::from(speed);
        let pitch = if controls.keylock() {
            1.0
        } else {
            f64::from(speed)
        };
        self.apply_streaming_pitch(pitch)?;
        let channels = self.channels();
        let frames = chunk.frames();
        let samples = &chunk.samples;
        let mut consumed = 0_usize;
        let mut frame = chunk.meta.frame_offset;
        while consumed < frames {
            let (region, crossed) = self.streaming_region_for(frame);
            let left = u64::try_from(frames - consumed).unwrap_or(u64::MAX);
            let span = region.end().saturating_sub(frame).min(left).max(1);
            let sub = usize::try_from(span).unwrap_or(frames - consumed);
            self.apply_streaming_stretch(base * region.correction(), crossed)?;
            let needed = self
                .scratch
                .len()
                .saturating_add(self.engine.max_output_samples(sub));
            if self.scratch.capacity() < needed {
                self.scratch.reserve(needed - self.scratch.len());
            }
            let part = &samples[consumed * channels..(consumed + sub) * channels];
            StretchBackend::process(&mut self.engine, part, &mut self.scratch)
                .map_err(|error| DecodeError::pcm_stream("resident tempo renderer", error))?;
            self.streaming_source_frames = self
                .streaming_source_frames
                .saturating_add(u64::try_from(sub).unwrap_or(u64::MAX));
            consumed += sub;
            frame = frame.saturating_add(span);
        }
        Ok(self.emit())
    }

    fn apply_streaming_pitch(&mut self, pitch: f64) -> DecodeResult<()>
    where
        E: StretchBackend,
    {
        if !self.streaming_pitch.is_nan() && (pitch - self.streaming_pitch).abs() <= Self::RATIO_EPS
        {
            return Ok(());
        }
        self.engine
            .set_pitch(pitch)
            .map_err(|error| DecodeError::pcm_stream("resident tempo renderer pitch", error))?;
        self.streaming_pitch = pitch;
        Ok(())
    }

    fn apply_streaming_stretch(&mut self, stretch: f64, boundary: bool) -> DecodeResult<()>
    where
        E: StretchBackend,
    {
        let first = self.streaming_stretch.is_nan();
        if !first && (stretch - self.streaming_stretch).abs() <= Self::RATIO_EPS {
            return Ok(());
        }
        if boundary && !first {
            StretchBackend::flush(&mut self.engine, &mut self.scratch)
                .map_err(|error| DecodeError::pcm_stream("resident tempo renderer flush", error))?;
            StretchBackend::reset(&mut self.engine);
            self.streaming_source_frames = 0;
        }
        self.engine
            .set_ratio(stretch)
            .map_err(|error| DecodeError::pcm_stream("resident tempo renderer stretch", error))?;
        self.streaming_stretch = stretch;
        Ok(())
    }

    fn reset_streaming_passthrough(&mut self)
    where
        E: StretchBackend,
    {
        if !self.streaming_active {
            return;
        }
        StretchBackend::reset(&mut self.engine);
        self.streaming_active = false;
        self.streaming_pitch = f64::NAN;
        self.streaming_source_frames = 0;
        self.streaming_stretch = f64::NAN;
    }

    fn streaming_region_for(&mut self, frame: u64) -> (ActiveRegion, bool) {
        if let Some(region) = self.streaming_region
            && region.contains(frame)
        {
            return (region, false);
        }
        let next = self
            .streaming_plan
            .as_ref()
            .map_or(ActiveRegion::UNBOUNDED, |plan| plan.region_at(frame));
        let crossed = self.streaming_region.is_some();
        self.streaming_region = Some(next);
        (next, crossed)
    }

    fn sync_streaming_plan(&mut self, controls: &StretchControls) {
        let target = controls.region_plan();
        let unchanged = match (&self.streaming_plan, &target) {
            (None, None) => true,
            (Some(active), Some(target)) => Arc::ptr_eq(active, target),
            _ => false,
        };
        if !unchanged {
            self.streaming_plan = target;
            self.streaming_region = None;
        }
    }
}

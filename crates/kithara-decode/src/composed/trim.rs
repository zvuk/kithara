use std::num::{NonZeroU32, NonZeroU128};

use kithara_bufpool::{HasPool, SampleBuffer};
use kithara_platform::time::Duration;
use kithara_signal::{AudioChunkInfo, AudioSpec};

use super::{ComposedDecoder, state::SeekTrim};
use crate::{
    codec::FrameCodec,
    demuxer::Demuxer,
    error::{DecodeError, DecodeResult},
};

/// Per-channel sample count to drop from the head of the target packet
/// so the emitted chunk starts at the user's seek target rather than
/// at the packet boundary. Returns 0 when `frame_pts >= target` or
/// `sample_rate == 0`. Rounds to nearest sample (half-up) so the trim
/// matches `round(target_secs * sample_rate)` callers use to index
/// PCM by absolute sample frame.
pub(super) fn frames_to_trim(frame_pts: Duration, target: Duration, sample_rate: u32) -> u64 {
    if sample_rate == 0 || frame_pts >= target {
        return 0;
    }
    let delta_nanos = target.saturating_sub(frame_pts).as_nanos();
    let sr_u128 = u128::from(sample_rate);
    let frames_u128 = delta_nanos
        .saturating_mul(sr_u128)
        .saturating_add(500_000_000)
        / 1_000_000_000;
    u64::try_from(frames_u128).unwrap_or(u64::MAX)
}

pub(super) fn frames_to_trim_prefix(
    frame_pts: Duration,
    target: Duration,
    sample_rate: NonZeroU32,
) -> DecodeResult<u64> {
    const NANOS_PER_SECOND: NonZeroU128 = match NonZeroU128::new(1_000_000_000) {
        Some(value) => value,
        None => unreachable!(),
    };
    if frame_pts >= target {
        return Ok(0);
    }
    let spec = AudioSpec::new(1, sample_rate);
    let first = spec.frame_at(frame_pts)?;
    let basis = AudioChunkInfo {
        spec,
        frame_offset: first,
        frames: 1,
        timestamp: frame_pts,
        ..AudioChunkInfo::default()
    }
    .source_geometry()
    .ok_or(DecodeError::InvalidData {
        detail: "seek prefix PCM geometry overflow",
    })?;
    let target_numerator = target
        .as_nanos()
        .checked_mul(u128::from(sample_rate.get()))
        .ok_or(DecodeError::InvalidData {
            detail: "seek prefix source position overflow",
        })?;
    let (numerator, denominator) = basis
        .output_ratio_at_source(first, (target_numerator, NANOS_PER_SECOND))
        .ok_or(DecodeError::InvalidData {
            detail: "seek prefix inverse geometry overflow",
        })?;
    let position =
        u64::try_from(numerator / denominator.get()).map_err(|_| DecodeError::InvalidData {
            detail: "seek prefix physical position overflow",
        })?;
    Ok(position.saturating_sub(first))
}

impl<D, C, S> ComposedDecoder<D, C, S>
where
    D: Demuxer,
    C: FrameCodec,
    S: HasPool<f32>,
{
    pub(super) fn trim_seek_chunk(
        &mut self,
        buf: &mut SampleBuffer,
        frames: &mut u32,
        chunk_pts: &mut Duration,
        frame_end: Duration,
    ) -> DecodeResult<bool> {
        if let Some(intent) = self.pending_seek_target {
            let target = match intent {
                SeekTrim::Nearest(target) | SeekTrim::Prefix(target) => target,
            };
            let decoded_end = chunk_pts.saturating_add(
                self.codec
                    .spec()
                    .duration_for(u64::from(*frames))
                    .unwrap_or(Duration::from_nanos(u64::MAX)),
            );
            if (*frames == 0 && frame_end <= target)
                || (*frames > 0 && matches!(intent, SeekTrim::Nearest(_)) && decoded_end <= target)
            {
                return Ok(true);
            }
            if *frames > 0
                && *chunk_pts < target
                && self.trim_pcm_prefix(buf, frames, chunk_pts, intent, target)?
            {
                return Ok(true);
            }
            if *frames > 0 {
                self.pending_seek_target = None;
            }
        }
        Ok(false)
    }

    fn trim_pcm_prefix(
        &mut self,
        buf: &mut SampleBuffer,
        frames: &mut u32,
        chunk_pts: &mut Duration,
        intent: SeekTrim,
        target: Duration,
    ) -> DecodeResult<bool> {
        let live_spec = self.codec.spec();
        let trim_frames_u64 = match intent {
            SeekTrim::Nearest(_) => frames_to_trim(*chunk_pts, target, live_spec.sample_rate.get()),
            SeekTrim::Prefix(_) => {
                frames_to_trim_prefix(*chunk_pts, target, live_spec.sample_rate)?
            }
        }
        .min(u64::from(*frames));
        let trim_frames = u32::try_from(trim_frames_u64).unwrap_or(*frames);
        if trim_frames >= *frames {
            return Ok(true);
        }
        if trim_frames > 0 {
            let channels = usize::from(live_spec.channels);
            let trim_samples = usize::try_from(trim_frames)
                .unwrap_or(0)
                .saturating_mul(channels);
            let total_samples = usize::try_from(*frames)
                .unwrap_or(0)
                .saturating_mul(channels);
            if trim_samples < total_samples && trim_samples <= buf.len() {
                buf.copy_within(trim_samples..total_samples, 0);
                buf.truncate(total_samples - trim_samples);
                *frames = frames.saturating_sub(trim_frames);
                let origin = live_spec.frame_at(*chunk_pts)?;
                let position =
                    origin
                        .checked_add(u64::from(trim_frames))
                        .ok_or(DecodeError::InvalidData {
                            detail: "trimmed PCM position overflow",
                        })?;
                let elapsed = live_spec
                    .duration_for(position)?
                    .checked_sub(live_spec.duration_for(origin)?)
                    .ok_or(DecodeError::InvalidData {
                        detail: "trimmed PCM clock moved backward",
                    })?;
                *chunk_pts = chunk_pts
                    .checked_add(elapsed)
                    .ok_or(DecodeError::InvalidData {
                        detail: "trimmed PCM timestamp overflow",
                    })?;
                self.frame_offset = position;
                self.resync_frame_offset_to_pts = false;
            }
        }

        Ok(false)
    }
}

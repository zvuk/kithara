use kithara_decode::{PcmChunk, PcmSpec};
use kithara_platform::time::Duration;
use kithara_stream::StreamType;

use crate::pipeline::{
    stream::shared::SharedStream,
    track_fsm::{DecoderSession, ResumeState},
};

struct Consts;

impl Consts {
    const NANOS_PER_SEC: u128 = 1_000_000_000;
}

pub(crate) fn duration(spec: PcmSpec, frames: usize) -> Duration {
    let nanos = (frames as u128)
        .saturating_mul(Consts::NANOS_PER_SEC)
        .saturating_div(u128::from(spec.sample_rate.get()));
    let nanos = num_traits::cast::ToPrimitive::to_u64(&nanos).unwrap_or(u64::MAX);
    Duration::from_nanos(nanos)
}

pub(crate) fn frames(spec: PcmSpec, duration: Duration) -> usize {
    let frames = duration
        .as_nanos()
        .saturating_mul(u128::from(spec.sample_rate.get()))
        .saturating_div(Consts::NANOS_PER_SEC);
    assert!(
        frames <= usize::MAX as u128,
        "post-seek frame count {frames} exceeds usize::MAX for {duration:?} at {} Hz",
        spec.sample_rate
    );
    frames as usize
}

pub(crate) fn estimate_target_byte<T: StreamType>(
    session: &DecoderSession,
    stream: &SharedStream<T>,
    position: Duration,
) -> Option<u64> {
    let duration = session.decoder.duration()?;
    let len = stream.len()?;
    if duration.is_zero() || len <= session.base_offset {
        return None;
    }
    let payload = len - session.base_offset;
    let relative = u64::try_from(
        position
            .as_nanos()
            .saturating_mul(u128::from(payload))
            .saturating_div(duration.as_nanos().max(1)),
    )
    .expect("seek target byte fits u64")
    .min(payload);
    Some(session.base_offset.saturating_add(relative))
}

pub(crate) fn apply(
    mut chunk: PcmChunk,
    epoch: u64,
    resume: Option<&mut ResumeState>,
) -> Option<PcmChunk> {
    let Some(resume) = resume else {
        return Some(chunk);
    };
    let Some(remaining) = resume.skip else {
        return Some(chunk);
    };
    if resume.seek.epoch != epoch || remaining.is_zero() {
        resume.skip = None;
        return Some(chunk);
    }
    let spec = chunk.spec();
    let channels = usize::from(spec.channels.max(1));
    let chunk_frames = chunk.frames();
    if chunk_frames == 0 {
        return None;
    }
    let mut drop_frames = frames(spec, remaining);
    if drop_frames == 0 {
        drop_frames = 1;
    }
    if drop_frames >= chunk_frames {
        let remaining = remaining.saturating_sub(duration(spec, chunk_frames));
        resume.skip = (!remaining.is_zero()).then_some(remaining);
        return None;
    }
    let drop_samples = drop_frames.saturating_mul(channels);
    let len = chunk.samples.len();
    chunk.samples.copy_within(drop_samples..len, 0);
    chunk.samples.truncate(len - drop_samples);
    chunk.meta.frame_offset = chunk.meta.frame_offset.saturating_add(drop_frames as u64);
    chunk.meta.timestamp = chunk
        .meta
        .timestamp
        .saturating_add(duration(spec, drop_frames));
    chunk.meta.frames = chunk
        .meta
        .frames
        .saturating_sub(u32::try_from(drop_frames).unwrap_or(u32::MAX));
    resume.skip = None;
    Some(chunk)
}

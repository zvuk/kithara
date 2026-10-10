use std::num::NonZeroU128;

use kithara_decode::{DecodeError, DecodeResult};
use kithara_platform::time::Duration;
use kithara_signal::{AudioChunk, AudioSpec, SourceSpan};
use tracing::debug;

use crate::{
    consts,
    pipeline::seek::{ResumeState, ResumeTarget},
};

pub(crate) fn frames(spec: AudioSpec, duration: Duration) -> usize {
    let frames = duration
        .as_nanos()
        .saturating_mul(u128::from(spec.sample_rate.get()))
        .saturating_div(consts::NANOS_PER_SEC);
    assert!(
        frames <= usize::MAX as u128,
        "post-seek frame count {frames} exceeds usize::MAX for {duration:?} at {} Hz",
        spec.sample_rate
    );
    frames as usize
}

pub(crate) fn apply(
    mut chunk: AudioChunk,
    resume: Option<&mut ResumeState>,
) -> DecodeResult<Option<AudioChunk>> {
    let Some(resume) = resume else {
        return Ok(Some(chunk));
    };
    if !resume.trim_head {
        return Ok(Some(chunk));
    }
    let spec = chunk.spec();
    let chunk_frames = chunk.frames();
    if chunk_frames == 0 {
        return Ok(None);
    }
    let drop_frames = match resume.target {
        ResumeTarget::Position(target) => frames(spec, target.saturating_sub(chunk.meta.timestamp)),
        ResumeTarget::Source(end) => {
            let target = output_frame(end, spec)?;
            usize::try_from(target.saturating_sub(chunk.meta.frame_offset))
                .map_err(|_| mapping_error())?
        }
    };
    if drop_frames >= chunk_frames {
        return Ok(None);
    }
    debug!(
        target = ?resume.target,
        chunk_at = ?chunk.meta.timestamp,
        frame_offset = chunk.meta.frame_offset,
        drop_frames,
        "trimmed the head of a resumed generation"
    );
    trim_start(&mut chunk, drop_frames)?;
    resume.trim_head = false;
    Ok(Some(chunk))
}

pub(crate) fn rebase_source(
    chunk: &mut AudioChunk,
    resume: Option<&ResumeState>,
) -> DecodeResult<()> {
    if let Some(ResumeState {
        target: ResumeTarget::Source(end),
        ..
    }) = resume
    {
        let spec = chunk.spec();
        let output_rate = u128::from(spec.sample_rate.get());
        let source_rate = u128::from(end.sample_rate().get());
        let offset = chunk
            .meta
            .frame_offset
            .checked_sub(output_frame(*end, spec)?)
            .ok_or_else(mapping_error)?;
        let start = u128::from(end.frame()) * output_rate + u128::from(offset) * source_rate;
        let span = SourceSpan::try_from((
            start,
            source_rate,
            NonZeroU128::from(spec.sample_rate),
            end.sample_rate(),
            u64::from(chunk.meta.frames),
        ))
        .ok()
        .ok_or_else(mapping_error)?
        .with_mapping_revision(end.mapping_revision())
        .with_render_revision(chunk.meta.render_revision);
        chunk.meta.timestamp = span.position_at(0).ok_or_else(mapping_error)?;
        chunk.meta.end_timestamp = span
            .position_at(span.output_frames())
            .ok_or_else(mapping_error)?;
        chunk.meta.mapping_revision = span.mapping_revision();
        chunk.meta.source_span = Some(span);
    }
    Ok(())
}

fn output_frame(end: crate::SourceEnd, spec: AudioSpec) -> DecodeResult<u64> {
    u64::try_from(
        u128::from(end.frame()) * u128::from(spec.sample_rate.get())
            / u128::from(end.sample_rate().get()),
    )
    .map_err(|_| mapping_error())
}

fn mapping_error() -> DecodeError {
    DecodeError::InvalidData {
        detail: "resumed source mapping does not cover PCM",
    }
}

pub(crate) fn apply_frames(
    mut chunk: AudioChunk,
    remaining: &mut u64,
) -> DecodeResult<Option<AudioChunk>> {
    if *remaining == 0 {
        return Ok(Some(chunk));
    }
    let chunk_frames = u64::try_from(chunk.frames()).unwrap_or(u64::MAX);
    if chunk_frames <= *remaining {
        *remaining = remaining.saturating_sub(chunk_frames);
        return Ok(None);
    }
    let drop_frames = usize::try_from(*remaining).unwrap_or(usize::MAX);
    trim_start(&mut chunk, drop_frames)?;
    *remaining = 0;
    Ok(Some(chunk))
}

fn trim_start(chunk: &mut AudioChunk, drop_frames: usize) -> DecodeResult<()> {
    let spec = chunk.spec();
    let frame_offset = chunk
        .meta
        .frame_offset
        .checked_add(drop_frames as u64)
        .ok_or_else(mapping_error)?;
    let source_span = chunk
        .meta
        .source_span
        .map(|span| {
            span.for_output_range(drop_frames as u64..u64::from(chunk.meta.frames))
                .ok_or_else(mapping_error)
        })
        .transpose()?;
    let timestamp = if let Some(span) = source_span {
        span.position_at(0).ok_or_else(mapping_error)?
    } else {
        let origin = chunk
            .meta
            .timestamp
            .saturating_sub(spec.duration_for(chunk.meta.frame_offset)?);
        origin
            .checked_add(spec.duration_for(frame_offset)?)
            .ok_or_else(mapping_error)?
    };
    let channels = usize::from(spec.channels.max(1));
    let drop_samples = drop_frames.saturating_mul(channels);
    let len = chunk.samples.len();
    chunk.samples.copy_within(drop_samples..len, 0);
    chunk.samples.truncate(len - drop_samples);
    chunk.meta.frame_offset = frame_offset;
    chunk.meta.timestamp = timestamp;
    chunk.meta.source_span = source_span;
    chunk.meta.frames = chunk
        .meta
        .frames
        .saturating_sub(u32::try_from(drop_frames).unwrap_or(u32::MAX));
    Ok(())
}

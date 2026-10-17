use std::ops::Range;

use kithara_platform::time::Duration;
use kithara_signal::AudioChunk;
use smallvec::SmallVec;

use super::core::GaplessOutput;
use crate::{DecodeError, DecodeResult};

/// Buffered PCM and its frame count have one owner through trimming and release.
#[derive(Debug, Default, fieldwork::Fieldwork)]
#[fieldwork(get, vis = "pub(super)")]
pub(super) struct BufferedChunks {
    #[field(get_mut, deref = "[AudioChunk]")]
    chunks: SmallVec<[AudioChunk; 4]>,
    frames: u64,
}

impl BufferedChunks {
    pub(super) fn clear(&mut self) {
        self.chunks.clear();
        self.frames = 0;
    }

    pub(super) fn push(&mut self, chunk: AudioChunk) {
        self.frames = self.frames.saturating_add(chunk_frames(&chunk));
        self.chunks.push(chunk);
    }

    pub(super) fn release(&mut self, trailing_frames: u64) -> GaplessOutput {
        let mut remaining_frames = self.frames;
        let mut release_count = 0;
        for chunk in &self.chunks {
            let next_remaining = remaining_frames.saturating_sub(chunk_frames(chunk));
            if next_remaining < trailing_frames {
                break;
            }
            remaining_frames = next_remaining;
            release_count += 1;
        }
        let mut ready = GaplessOutput::new();
        ready.extend(self.chunks.drain(..release_count));
        self.frames = remaining_frames;
        ready
    }

    pub(super) fn trim(&mut self, trim_frames: u64) -> DecodeResult<()> {
        let mut drop_frames = trim_frames.min(self.frames);
        while drop_frames > 0 {
            let Some(back) = self.chunks.last_mut() else {
                break;
            };
            let back_frames = chunk_frames(back);
            if back_frames <= drop_frames {
                drop_frames -= back_frames;
                self.frames = self.frames.saturating_sub(back_frames);
                self.chunks.pop();
                continue;
            }
            trim_chunk_end(back, drop_frames)?;
            self.frames = self.frames.saturating_sub(drop_frames);
            drop_frames = 0;
        }
        Ok(())
    }

    pub(super) fn take(&mut self) -> SmallVec<[AudioChunk; 4]> {
        self.frames = 0;
        std::mem::take(&mut self.chunks)
    }

    pub(super) fn drain(&mut self) -> GaplessOutput {
        let mut ready = GaplessOutput::new();
        ready.extend(self.take());
        ready
    }
}

pub(super) fn trim_leading(
    mut chunk: AudioChunk,
    leading_remaining: &mut u64,
) -> DecodeResult<Option<AudioChunk>> {
    if *leading_remaining > 0 {
        let chunk_frames = chunk_frames(&chunk);
        if chunk_frames <= *leading_remaining {
            *leading_remaining -= chunk_frames;
            return Ok(None);
        }

        let trim_frames = usize_from_u64_saturating(*leading_remaining);
        *leading_remaining = 0;
        trim_chunk_start(&mut chunk, trim_frames)?;
    }

    Ok(Some(chunk))
}

fn trim_chunk_start(chunk: &mut AudioChunk, trim_frames: usize) -> DecodeResult<()> {
    let mapped = trim_source_span(chunk, trim_frames as u64..chunk_frames(chunk))?;
    let spec = chunk.spec();
    let channels = usize::from(spec.channels.max(1));
    let trim_samples = trim_frames.saturating_mul(channels);
    let len = chunk.samples.len();
    chunk.samples.copy_within(trim_samples..len, 0);
    chunk.samples.truncate(len.saturating_sub(trim_samples));
    let original_offset = chunk.meta.frame_offset;
    chunk.meta.frame_offset = original_offset.saturating_add(trim_frames as u64);
    chunk.meta.frames = u32::try_from(chunk.samples.len() / channels.max(1)).unwrap_or(u32::MAX);
    if mapped {
        return Ok(());
    }
    let trim_duration = spec
        .duration_for(chunk.meta.frame_offset)
        .and_then(|end| {
            spec.duration_for(original_offset)
                .map(|start| end.saturating_sub(start))
        })
        .unwrap_or(Duration::from_nanos(u64::MAX));
    chunk.meta.timestamp = chunk.meta.timestamp.saturating_add(trim_duration);
    Ok(())
}

fn trim_chunk_end(chunk: &mut AudioChunk, trim_frames: u64) -> DecodeResult<()> {
    let channels = usize::from(chunk.spec().channels.max(1));
    let keep_frames = usize_from_u64_saturating(chunk_frames(chunk).saturating_sub(trim_frames));
    let keep_samples = keep_frames.saturating_mul(channels);
    trim_source_span(chunk, 0..keep_frames as u64)?;
    chunk.samples.truncate(keep_samples);
    chunk.meta.frames = u32::try_from(keep_frames).unwrap_or(u32::MAX);
    Ok(())
}

pub(super) fn validate_geometry(chunk: &AudioChunk) -> DecodeResult<()> {
    if let Some(span) = chunk.meta.source_span
        && (span.output_frames() != chunk_frames(chunk)
            || u64::from(chunk.meta.frames) != chunk_frames(chunk))
    {
        return Err(geometry_error());
    }
    Ok(())
}

fn trim_source_span(chunk: &mut AudioChunk, range: Range<u64>) -> DecodeResult<bool> {
    let Some(span) = chunk.meta.source_span else {
        return Ok(false);
    };
    validate_geometry(chunk)?;
    let span = span.for_output_range(range).ok_or_else(geometry_error)?;
    let timestamp = span.position_at(0).ok_or_else(geometry_error)?;
    let end_timestamp = span
        .position_at(span.output_frames())
        .ok_or_else(geometry_error)?;
    chunk.meta.source_span = Some(span);
    chunk.meta.timestamp = timestamp;
    chunk.meta.end_timestamp = end_timestamp;
    Ok(true)
}

fn geometry_error() -> DecodeError {
    DecodeError::InvalidData {
        detail: "gapless PCM has invalid source geometry",
    }
}

pub(super) fn output_with(chunk: AudioChunk) -> GaplessOutput {
    let mut ready = GaplessOutput::new();
    ready.push(chunk);
    ready
}

pub(super) fn chunk_frames(chunk: &AudioChunk) -> u64 {
    u64::try_from(chunk.frames()).unwrap_or(u64::MAX)
}

pub(super) fn usize_from_u64_saturating(value: u64) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

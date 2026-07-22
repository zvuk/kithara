use std::collections::VecDeque;

use kithara_bufpool::PcmPool;
use kithara_decode::{
    DecodeError, DecodeResult, PcmChunk, duration_for_frames, frames_for_duration,
};
use kithara_platform::time::Duration;
use num_traits::cast::ToPrimitive;
use tracing::debug;

pub(crate) struct GaplessBlender {
    pool: PcmPool,
    duration: Duration,
    held: VecDeque<PcmChunk>,
    held_frames: usize,
    ready: VecDeque<PcmChunk>,
    transition: Option<Transition>,
}

struct Transition {
    outgoing: VecDeque<PcmChunk>,
    incoming: VecDeque<PcmChunk>,
    frames_total: usize,
    frames_blended: usize,
}

impl GaplessBlender {
    pub(crate) fn new(pool: PcmPool, duration: Duration) -> Self {
        Self {
            pool,
            duration,
            held: VecDeque::new(),
            held_frames: 0,
            ready: VecDeque::new(),
            transition: None,
        }
    }

    pub(crate) fn set_duration(&mut self, duration: Duration) {
        self.duration = duration;
    }

    pub(crate) fn push(&mut self, chunk: PcmChunk) -> DecodeResult<()> {
        if self.transition.is_some() {
            return self.push_transition(chunk);
        }
        self.push_normal(chunk)
    }

    #[must_use]
    pub(crate) fn next(&mut self) -> Option<PcmChunk> {
        self.ready.pop_front()
    }

    pub(crate) fn flush(&mut self) {
        self.ready.append(&mut self.held);
        self.held_frames = 0;
    }

    pub(crate) fn reset(&mut self) {
        self.held.clear();
        self.held_frames = 0;
        self.ready.clear();
        self.transition = None;
    }

    #[must_use]
    pub(crate) fn transition_active(&self) -> bool {
        self.transition.is_some()
    }

    pub(crate) fn begin_transition(&mut self) -> DecodeResult<Option<Duration>> {
        if self.transition.is_some() {
            return Err(DecodeError::InvalidData {
                detail: "decoder transition began before prior blend completed",
            });
        }
        let Some(start) = self.held.front().map(|chunk| chunk.meta.timestamp) else {
            return Ok(None);
        };
        if self.held_frames == 0 {
            return Ok(None);
        }
        let outgoing = std::mem::take(&mut self.held);
        let frames_total = self.held_frames;
        self.held_frames = 0;
        self.transition = Some(Transition {
            outgoing,
            incoming: VecDeque::new(),
            frames_total,
            frames_blended: 0,
        });
        debug!(?start, frames_total, "starting decoder PCM blend");
        Ok(Some(start))
    }

    fn push_normal(&mut self, chunk: PcmChunk) -> DecodeResult<()> {
        let keep_frames = frames_for_duration(chunk.spec().sample_rate.get(), self.duration);
        if keep_frames == 0 {
            self.ready.push_back(chunk);
            return Ok(());
        }
        self.held_frames = self.held_frames.saturating_add(chunk.frames());
        self.held.push_back(chunk);
        while self.held_frames > keep_frames {
            let release = self.held_frames.saturating_sub(keep_frames);
            let chunk = take_front(&mut self.held, release, &self.pool)?;
            self.held_frames = self.held_frames.saturating_sub(chunk.frames());
            self.ready.push_back(chunk);
        }
        Ok(())
    }

    fn push_transition(&mut self, chunk: PcmChunk) -> DecodeResult<()> {
        let transition = self.transition.as_mut().ok_or(DecodeError::InvalidData {
            detail: "gapless blender transition state missing",
        })?;
        transition.incoming.push_back(chunk);
        while let (Some(outgoing), Some(incoming)) =
            (transition.outgoing.front(), transition.incoming.front())
        {
            if outgoing.spec() != incoming.spec() {
                return Err(DecodeError::InvalidData {
                    detail: "decoder transition PCM specifications differ",
                });
            }
            let frames = outgoing.frames().min(incoming.frames());
            let outgoing = take_front(&mut transition.outgoing, frames, &self.pool)?;
            let incoming = take_front(&mut transition.incoming, frames, &self.pool)?;
            if transition.frames_blended == 0 {
                debug!(
                    outgoing_start = ?outgoing.meta.timestamp,
                    incoming_start = ?incoming.meta.timestamp,
                    "mixing decoder PCM transition"
                );
            }
            let mixed = mix(&self.pool, &outgoing, &incoming, transition)?;
            self.ready.push_back(mixed);
        }
        if self
            .transition
            .as_ref()
            .is_some_and(|transition| transition.outgoing.is_empty())
        {
            let transition = self.transition.take().ok_or(DecodeError::InvalidData {
                detail: "gapless blender completed transition missing",
            })?;
            for chunk in transition.incoming {
                self.push_normal(chunk)?;
            }
        }
        Ok(())
    }
}

fn take_front(
    chunks: &mut VecDeque<PcmChunk>,
    frames: usize,
    pool: &PcmPool,
) -> DecodeResult<PcmChunk> {
    let mut chunk = chunks.pop_front().ok_or(DecodeError::InvalidData {
        detail: "gapless blender missing PCM chunk",
    })?;
    if frames >= chunk.frames() {
        return Ok(chunk);
    }
    let tail = split_after(&mut chunk, frames, pool)?;
    chunks.push_front(tail);
    Ok(chunk)
}

fn split_after(chunk: &mut PcmChunk, frames: usize, pool: &PcmPool) -> DecodeResult<PcmChunk> {
    let spec = chunk.spec();
    let channels = usize::from(spec.channels);
    let head_samples = frames.saturating_mul(channels);
    let mut samples = pool.get();
    samples
        .ensure_len(chunk.samples.len().saturating_sub(head_samples))
        .map_err(|_| DecodeError::InvalidData {
            detail: "gapless blender PCM pool exhausted",
        })?;
    samples.copy_from_slice(&chunk.samples[head_samples..]);
    let mut tail_meta = chunk.meta;
    let offset = duration_for_frames(
        spec.sample_rate.get(),
        u64::try_from(frames).unwrap_or(u64::MAX),
    );
    tail_meta.timestamp = tail_meta.timestamp.saturating_add(offset);
    tail_meta.frame_offset = tail_meta
        .frame_offset
        .saturating_add(u64::try_from(frames).unwrap_or(u64::MAX));
    tail_meta.frames = u32::try_from(chunk.frames().saturating_sub(frames)).unwrap_or(u32::MAX);
    tail_meta.source_byte_offset = None;
    tail_meta.source_bytes = 0;
    chunk.samples.truncate(head_samples);
    chunk.meta.frames = u32::try_from(frames).unwrap_or(u32::MAX);
    chunk.meta.end_timestamp = chunk.meta.timestamp.saturating_add(offset);
    chunk.meta.source_byte_offset = None;
    chunk.meta.source_bytes = 0;
    Ok(PcmChunk::new(tail_meta, samples))
}

fn mix(
    pool: &PcmPool,
    outgoing: &PcmChunk,
    incoming: &PcmChunk,
    transition: &mut Transition,
) -> DecodeResult<PcmChunk> {
    let frames = outgoing.frames();
    let channels = usize::from(outgoing.spec().channels);
    let mut samples = pool.get();
    samples
        .ensure_len(outgoing.samples.len())
        .map_err(|_| DecodeError::InvalidData {
            detail: "gapless blender PCM pool exhausted",
        })?;
    let denominator = transition.frames_total.saturating_sub(1).max(1);
    for frame in 0..frames {
        let position = transition.frames_blended.saturating_add(frame);
        let incoming_gain =
            position.to_f32().unwrap_or(f32::MAX) / denominator.to_f32().unwrap_or(f32::MAX);
        let outgoing_gain = 1.0 - incoming_gain;
        let start = frame.saturating_mul(channels);
        let end = start.saturating_add(channels);
        samples[start..end]
            .iter_mut()
            .zip(
                outgoing.samples[start..end]
                    .iter()
                    .zip(incoming.samples[start..end].iter()),
            )
            .for_each(|(sample, (old, new))| {
                *sample = old.mul_add(outgoing_gain, new * incoming_gain);
            });
    }
    transition.frames_blended = transition.frames_blended.saturating_add(frames);
    Ok(PcmChunk::new(outgoing.meta, samples))
}

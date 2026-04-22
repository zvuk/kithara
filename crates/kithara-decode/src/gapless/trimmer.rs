use std::{collections::VecDeque, time::Duration};

use smallvec::SmallVec;

use crate::{GaplessInfo, PcmChunk, PcmSpec};

/// Stateful PCM trimmer that applies one track's gapless contract.
///
/// It drops `leading_frames` from the beginning of the stream and buffers
/// the final chunks so `trailing_frames` can be removed only at EOF.
#[derive(Debug, Default)]
pub struct GaplessTrimmer {
    leading_remaining: u64,
    trailing_to_keep: u64,
    enabled: bool,
    tail_buffer: VecDeque<PcmChunk>,
    tail_buffered_frames: u64,
}

impl GaplessTrimmer {
    #[must_use]
    pub fn from_info(info: GaplessInfo) -> Self {
        let enabled = info.leading_frames > 0 || info.trailing_frames > 0;
        Self {
            leading_remaining: info.leading_frames,
            trailing_to_keep: info.trailing_frames,
            enabled,
            tail_buffer: VecDeque::new(),
            tail_buffered_frames: 0,
        }
    }

    #[must_use]
    pub fn disabled() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn push(&mut self, mut chunk: PcmChunk) -> SmallVec<[PcmChunk; 2]> {
        if !self.enabled {
            let mut ready = SmallVec::new();
            ready.push(chunk);
            return ready;
        }

        if self.leading_remaining > 0 {
            let chunk_frames = chunk.frames() as u64;
            if chunk_frames <= self.leading_remaining {
                self.leading_remaining -= chunk_frames;
                return SmallVec::new();
            }

            let trim_frames = self.leading_remaining.min(chunk_frames) as usize;
            self.leading_remaining -= trim_frames as u64;
            trim_chunk_start(&mut chunk, trim_frames);
        }

        let chunk_frames = chunk.frames() as u64;
        if chunk_frames == 0 {
            return SmallVec::new();
        }

        self.tail_buffered_frames = self.tail_buffered_frames.saturating_add(chunk_frames);
        self.tail_buffer.push_back(chunk);

        let mut ready = SmallVec::new();
        while let Some(front) = self.tail_buffer.front() {
            let front_frames = front.frames() as u64;
            let remaining_after_pop = self.tail_buffered_frames.saturating_sub(front_frames);
            if remaining_after_pop < self.trailing_to_keep {
                break;
            }

            self.tail_buffered_frames = remaining_after_pop;
            if let Some(front) = self.tail_buffer.pop_front() {
                ready.push(front);
            }
        }

        ready
    }

    #[must_use]
    pub fn flush(&mut self) -> SmallVec<[PcmChunk; 2]> {
        if !self.enabled {
            return SmallVec::new();
        }

        let mut drop_frames = self.trailing_to_keep.min(self.tail_buffered_frames);
        while drop_frames > 0 {
            let Some(back) = self.tail_buffer.back_mut() else {
                break;
            };
            let chunk_frames = back.frames() as u64;
            if chunk_frames <= drop_frames {
                drop_frames -= chunk_frames;
                self.tail_buffered_frames = self.tail_buffered_frames.saturating_sub(chunk_frames);
                self.tail_buffer.pop_back();
                continue;
            }

            let keep_frames = chunk_frames.saturating_sub(drop_frames) as usize;
            let keep_samples = keep_frames.saturating_mul(usize::from(back.spec().channels.max(1)));
            back.pcm.truncate(keep_samples);
            self.tail_buffered_frames = self.tail_buffered_frames.saturating_sub(drop_frames);
            drop_frames = 0;
        }

        let mut ready = SmallVec::new();
        while let Some(chunk) = self.tail_buffer.pop_front() {
            self.tail_buffered_frames = self
                .tail_buffered_frames
                .saturating_sub(chunk.frames() as u64);
            ready.push(chunk);
        }

        ready
    }

    pub fn notify_seek(&mut self) {
        self.leading_remaining = 0;
        self.tail_buffer.clear();
        self.tail_buffered_frames = 0;
    }
}

fn trim_chunk_start(chunk: &mut PcmChunk, trim_frames: usize) {
    let spec = chunk.spec();
    let channels = usize::from(spec.channels.max(1));
    let trim_samples = trim_frames.saturating_mul(channels);
    let len = chunk.pcm.len();
    chunk.pcm.copy_within(trim_samples..len, 0);
    chunk.pcm.truncate(len.saturating_sub(trim_samples));
    chunk.meta.frame_offset = chunk.meta.frame_offset.saturating_add(trim_frames as u64);
    chunk.meta.timestamp = chunk
        .meta
        .timestamp
        .saturating_add(duration_for_frames(spec, trim_frames));
}

fn duration_for_frames(spec: PcmSpec, frames: usize) -> Duration {
    if spec.sample_rate == 0 {
        return Duration::ZERO;
    }

    let nanos = (frames as u128)
        .saturating_mul(1_000_000_000)
        .saturating_div(u128::from(spec.sample_rate));
    Duration::from_nanos(nanos.min(u128::from(u64::MAX)) as u64)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use kithara_bufpool::pcm_pool;
    use kithara_test_utils::kithara;

    use crate::{GaplessInfo, PcmChunk, PcmMeta, PcmSpec};

    use super::GaplessTrimmer;

    fn chunk(spec: PcmSpec, frame_offset: u64, frames: usize) -> PcmChunk {
        let samples = frames.saturating_mul(usize::from(spec.channels));
        let pcm = (0..samples).map(|idx| idx as f32).collect::<Vec<_>>();
        PcmChunk::new(
            PcmMeta {
                spec,
                frame_offset,
                ..Default::default()
            },
            pcm_pool().attach(pcm),
        )
    }

    fn mono_spec() -> PcmSpec {
        PcmSpec {
            channels: 1,
            sample_rate: 48_000,
        }
    }

    #[kithara::test]
    fn leading_trim_updates_offset_and_timestamp() {
        let spec = mono_spec();
        let mut trimmer = GaplessTrimmer::from_info(GaplessInfo {
            leading_frames: 576,
            trailing_frames: 0,
        });

        let mut ready = trimmer.push(chunk(spec, 0, 1024));
        assert_eq!(ready.len(), 1);
        let out = ready.remove(0);
        assert_eq!(out.frames(), 448);
        assert_eq!(out.meta.frame_offset, 576);
        assert_eq!(out.meta.timestamp, Duration::from_millis(12));
        assert_eq!(out.samples()[0], 576.0);
    }

    #[kithara::test]
    fn leading_trim_can_consume_multiple_chunks() {
        let spec = mono_spec();
        let mut trimmer = GaplessTrimmer::from_info(GaplessInfo {
            leading_frames: 2400,
            trailing_frames: 0,
        });

        assert!(trimmer.push(chunk(spec, 0, 1024)).is_empty());
        assert!(trimmer.push(chunk(spec, 1024, 1024)).is_empty());

        let mut ready = trimmer.push(chunk(spec, 2048, 1024));
        assert_eq!(ready.len(), 1);
        let out = ready.remove(0);
        assert_eq!(out.frames(), 672);
        assert_eq!(out.meta.frame_offset, 2400);
        assert_eq!(out.samples()[0], 352.0);
    }

    #[kithara::test]
    fn trailing_trim_buffers_until_flush() {
        let spec = mono_spec();
        let mut trimmer = GaplessTrimmer::from_info(GaplessInfo {
            leading_frames: 0,
            trailing_frames: 64,
        });

        assert!(trimmer.push(chunk(spec, 0, 32)).is_empty());

        let mut ready = trimmer.push(chunk(spec, 32, 64));
        assert_eq!(ready.len(), 1);
        assert_eq!(ready.remove(0).frames(), 32);

        let ready = trimmer.flush();
        assert!(ready.is_empty());
    }

    #[kithara::test]
    fn trailing_trim_drops_tail_on_flush() {
        let spec = mono_spec();
        let mut trimmer = GaplessTrimmer::from_info(GaplessInfo {
            leading_frames: 0,
            trailing_frames: 2_048,
        });

        assert!(trimmer.push(chunk(spec, 0, 1_024)).is_empty());
        assert!(trimmer.push(chunk(spec, 1_024, 1_024)).is_empty());
        assert!(trimmer.push(chunk(spec, 2_048, 1_024)).len() == 1);

        let ready = trimmer.flush();
        assert!(ready.is_empty());
    }

    #[kithara::test]
    fn disabled_trimmer_passes_through() {
        let spec = mono_spec();
        let mut trimmer = GaplessTrimmer::disabled();
        let mut ready = trimmer.push(chunk(spec, 0, 128));
        assert_eq!(ready.len(), 1);
        assert_eq!(ready.remove(0).frames(), 128);
        assert!(trimmer.flush().is_empty());
    }

    #[kithara::test]
    fn notify_seek_resets_leading_only() {
        let spec = mono_spec();
        let mut trimmer = GaplessTrimmer::from_info(GaplessInfo {
            leading_frames: 128,
            trailing_frames: 64,
        });

        assert!(trimmer.push(chunk(spec, 0, 64)).is_empty());
        trimmer.notify_seek();

        assert!(trimmer.push(chunk(spec, 64, 128)).is_empty());

        let mut ready = trimmer.flush();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready.remove(0).frames(), 64);
    }
}

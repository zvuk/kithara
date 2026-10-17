use kithara_decode::DecodeResult;
use kithara_signal::{AudioChunk, AudioSpec};
use kithara_test_utils::kithara;

use super::generation::{
    DecoderGeneration, Holdback, StageFailure, StageOutput, StageProgress, StageResult,
    chunk_range, stage_failure,
};

impl DecoderGeneration {
    fn drain_holdback(&mut self) -> StageResult {
        self.pump_holdback(None)
    }

    fn holdback_progress(&self, cut: Option<u64>) -> StageProgress {
        let Some(holdback) = self.holdback else {
            return StageProgress::NeedMore;
        };
        let Some(front) = self.staged.front() else {
            return StageProgress::NeedMore;
        };
        let Some((first, frontier)) = chunk_range(front, holdback.spec) else {
            return StageProgress::NeedMore;
        };
        let Some(back) = self.staged.back() else {
            return StageProgress::NeedMore;
        };
        let Some((_, staged_end)) = chunk_range(back, holdback.spec) else {
            return StageProgress::NeedMore;
        };
        let join_start = cut.unwrap_or(frontier);
        let Some(join_end) = join_start.checked_add(holdback.join_frames) else {
            return StageProgress::NeedMore;
        };
        if first <= join_start && join_end <= staged_end {
            StageProgress::Ready
        } else {
            StageProgress::NeedMore
        }
    }

    pub(crate) fn next_with_holdback(&mut self) -> StageOutput {
        match self.pump_holdback(None) {
            StageResult::Ready => StageOutput::Output(self.staged.pop_front()),
            StageResult::NeedMore => StageOutput::Output(None),
            StageResult::Invalid(failure) => StageOutput::Invalid(*failure),
        }
    }

    pub(crate) fn prepare_holdback(&mut self, spec: AudioSpec, join_frames: u64) -> StageResult {
        self.holdback = None;
        let Some(slots) = usize::try_from(join_frames)
            .ok()
            .and_then(|frames| frames.checked_add(1))
        else {
            return StageResult::NeedMore;
        };
        if self.staged.capacity() < slots {
            self.staged.reserve_exact(slots - self.staged.len());
        }
        let holdback = Holdback {
            spec,
            join_frames,
            slots,
        };
        if let Some(failure) = self.validate_staged_holdback(holdback) {
            return StageResult::Invalid(Box::new(failure));
        }
        self.holdback = Some(holdback);
        self.drain_holdback()
    }

    #[kithara::hang_watchdog]
    fn pump_holdback(&mut self, cut: Option<u64>) -> StageResult {
        loop {
            hang_tick!();
            if let Some(cut) = cut
                && let Some(holdback) = self.holdback
            {
                while self.staged.get(1).is_some_and(|next| {
                    chunk_range(next, holdback.spec).is_some_and(|(_, end)| end <= cut)
                }) {
                    self.staged.pop_front();
                }
            }
            let progress = self.holdback_progress(cut);
            if matches!(progress, StageProgress::Ready) {
                return StageResult::Ready;
            }
            let Some(chunk) = self.gapless.next() else {
                return StageResult::NeedMore;
            };
            if let Some(failure) = self.push_holdback_chunk(chunk) {
                return StageResult::Invalid(Box::new(failure));
            }
            hang_reset!();
        }
    }

    pub(crate) fn push_holdback(
        &mut self,
        chunk: AudioChunk,
        cut: Option<u64>,
    ) -> DecodeResult<StageResult> {
        if self.gapless.has_output() {
            return Ok(StageResult::Invalid(Box::new(stage_failure(
                chunk,
                "transition holdback has undrained gapless output",
            ))));
        }
        if matches!(self.holdback_progress(cut), StageProgress::Ready) {
            return Ok(StageResult::Invalid(Box::new(stage_failure(
                chunk,
                "transition holdback is already ready",
            ))));
        }
        self.observe_comparison_base();
        self.gapless.push(chunk)?;
        Ok(self.pump_holdback(cut))
    }

    fn push_holdback_chunk(&mut self, chunk: AudioChunk) -> Option<StageFailure> {
        let Some(holdback) = self.holdback else {
            return Some(stage_failure(chunk, "transition holdback was not prepared"));
        };
        let Some((start, _)) = chunk_range(&chunk, holdback.spec) else {
            let detail = if chunk.spec() == holdback.spec {
                "invalid PCM metadata in transition holdback"
            } else {
                "mixed PCM spec in transition holdback"
            };
            return Some(stage_failure(chunk, detail));
        };
        if self.staged.len() >= holdback.slots {
            return Some(stage_failure(
                chunk,
                "transition holdback capacity exceeded",
            ));
        }
        if let Some(back) = self.staged.back() {
            let Some((_, staged_end)) = chunk_range(back, holdback.spec) else {
                return Some(stage_failure(
                    chunk,
                    "invalid retained PCM in transition holdback",
                ));
            };
            if staged_end != start {
                return Some(stage_failure(
                    chunk,
                    "discontinuous PCM in transition holdback",
                ));
            }
        }
        self.staged.push_back(chunk);
        None
    }

    fn validate_staged_holdback(&mut self, holdback: Holdback) -> Option<StageFailure> {
        #[cfg(test)]
        self.record_staged_scan();

        let mut staged_end = None;
        let mut invalid = None;
        for (index, chunk) in self.staged.iter().enumerate() {
            let Some((start, end)) = chunk_range(chunk, holdback.spec) else {
                let detail = if chunk.spec() == holdback.spec {
                    "invalid PCM metadata in transition holdback"
                } else {
                    "mixed PCM spec in transition holdback"
                };
                invalid = Some((index, detail));
                break;
            };
            if index >= holdback.slots {
                invalid = Some((index, "transition holdback capacity exceeded"));
                break;
            }
            if staged_end.is_some_and(|expected| expected != start) {
                invalid = Some((index, "discontinuous PCM in transition holdback"));
                break;
            }
            staged_end = Some(end);
        }
        if let Some((index, detail)) = invalid {
            return self
                .staged
                .remove(index)
                .map(|chunk| stage_failure(chunk, detail));
        }
        None
    }

    pub(crate) fn copy_staged_frames(
        &self,
        spec: AudioSpec,
        start: u64,
        frames: u64,
        output: &mut [f32],
    ) -> bool {
        let channels = usize::from(spec.channels);
        let Ok(frame_count) = usize::try_from(frames) else {
            return false;
        };
        let Some(sample_count) = frame_count.checked_mul(channels) else {
            return false;
        };
        if channels == 0 || output.len() != sample_count || !self.staged_covers(spec, start, frames)
        {
            return false;
        }
        let Some(end) = start.checked_add(frames) else {
            return false;
        };
        let mut written = 0usize;
        for chunk in &self.staged {
            let Some((chunk_start, chunk_end)) = chunk_range(chunk, spec) else {
                return false;
            };
            let copy_start = chunk_start.max(start);
            let copy_end = chunk_end.min(end);
            if copy_start >= copy_end {
                continue;
            }
            let Some(source_start) = usize::try_from(copy_start - chunk_start)
                .ok()
                .and_then(|frame| frame.checked_mul(channels))
            else {
                return false;
            };
            let Some(copy_samples) = usize::try_from(copy_end - copy_start)
                .ok()
                .and_then(|frame| frame.checked_mul(channels))
            else {
                return false;
            };
            let Some(source_end) = source_start.checked_add(copy_samples) else {
                return false;
            };
            let Some(output_end) = written.checked_add(copy_samples) else {
                return false;
            };
            let (Some(source), Some(destination)) = (
                chunk.samples.get(source_start..source_end),
                output.get_mut(written..output_end),
            ) else {
                return false;
            };
            destination.copy_from_slice(source);
            written = output_end;
        }
        written == output.len()
    }

    #[kithara::hang_watchdog]
    pub(crate) fn finish_staging(&mut self) -> DecodeResult<()> {
        self.finish()?;
        while let Some(chunk) = self.gapless.next() {
            hang_tick!();
            self.observe_visible_origin(&chunk);
            self.staged.push_back(chunk);
            hang_reset!();
        }
        Ok(())
    }

    pub(crate) fn has_output(&self) -> bool {
        !self.staged.is_empty() || self.gapless.has_output()
    }

    pub(crate) fn pop_staged(&mut self) -> Option<AudioChunk> {
        self.holdback = None;
        self.staged.pop_front()
    }

    pub(crate) fn push_staged_front(&mut self, chunk: AudioChunk) {
        self.holdback = None;
        self.staged.push_front(chunk);
    }

    #[kithara::hang_watchdog]
    pub(crate) fn stage(&mut self, chunk: AudioChunk) -> DecodeResult<()> {
        self.holdback = None;
        self.observe_comparison_base();
        self.gapless.push(chunk)?;
        while let Some(chunk) = self.gapless.next() {
            hang_tick!();
            self.observe_visible_origin(&chunk);
            self.staged.push_back(chunk);
            hang_reset!();
        }
        Ok(())
    }

    pub(crate) fn staged_covers(&self, spec: AudioSpec, start: u64, frames: u64) -> bool {
        let Some(end) = start.checked_add(frames) else {
            return false;
        };
        let Some((first, staged_end, staged_spec)) = self.staged_span() else {
            return false;
        };
        staged_spec == spec && first <= start && end <= staged_end
    }

    pub(crate) fn staged_span(&self) -> Option<(u64, u64, AudioSpec)> {
        #[cfg(test)]
        self.record_staged_scan();
        let first = self.staged.front()?;
        let spec = first.spec();
        let (start, mut end) = chunk_range(first, spec)?;
        for chunk in self.staged.iter().skip(1) {
            let (next, next_end) = chunk_range(chunk, spec)?;
            if next != end {
                return None;
            }
            end = next_end;
        }
        Some((start, end, spec))
    }
}

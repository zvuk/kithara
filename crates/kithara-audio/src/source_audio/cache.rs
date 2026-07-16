use std::{collections::VecDeque, num::NonZeroUsize};

use kithara_decode::PcmSpec;

use super::model::{SourceAudioError, SourceAudioWindow, SourceFrameRange, sample_count};

pub(crate) enum SourceAudioCacheInsert {
    Stored { evicted: Option<SourceAudioWindow> },
    Rejected { window: SourceAudioWindow },
}

pub(crate) struct SourceAudioCache {
    max_windows: NonZeroUsize,
    spec: Option<PcmSpec>,
    windows: VecDeque<SourceAudioWindow>,
}

impl SourceAudioCache {
    pub(crate) fn new(max_windows: NonZeroUsize) -> Self {
        Self {
            max_windows,
            spec: None,
            windows: VecDeque::with_capacity(max_windows.get()),
        }
    }

    pub(crate) fn insert(&mut self, window: SourceAudioWindow) -> SourceAudioCacheInsert {
        if self.spec.is_some_and(|spec| spec != window.spec()) || self.contains(window.range()) {
            return SourceAudioCacheInsert::Rejected { window };
        }

        let evicted = if self.windows.len() == self.max_windows.get() {
            self.windows.pop_front()
        } else {
            None
        };
        self.spec = Some(window.spec());
        self.windows.push_back(window);
        SourceAudioCacheInsert::Stored { evicted }
    }

    pub(crate) fn contains(&self, range: SourceFrameRange) -> bool {
        if range.is_empty() {
            return true;
        }
        let mut frame = range.start();
        while frame < range.end() {
            let Some(window) = self.window_at(frame) else {
                return false;
            };
            frame = window.range().end().min(range.end());
        }
        true
    }

    pub(crate) fn copy(
        &self,
        range: SourceFrameRange,
        output: &mut [f32],
    ) -> Result<(), SourceAudioError> {
        let spec = self.spec.ok_or(SourceAudioError::SpecMismatch)?;
        let expected = sample_count(range, spec)?;
        if output.len() != expected {
            return Err(SourceAudioError::OutputSizeMismatch {
                expected,
                actual: output.len(),
            });
        }

        let channels = usize::from(spec.channels);
        let mut frame = range.start();
        let mut output_offset = 0usize;
        while frame < range.end() {
            let window = self.window_at(frame).ok_or(SourceAudioError::StaleDemand)?;
            let copy_end = window.range().end().min(range.end());
            let source_frame_offset = usize::try_from(frame - window.range().start())
                .map_err(|_| SourceAudioError::SampleCountOverflow)?;
            let copy_frames = usize::try_from(copy_end - frame)
                .map_err(|_| SourceAudioError::SampleCountOverflow)?;
            let source_start = source_frame_offset
                .checked_mul(channels)
                .ok_or(SourceAudioError::SampleCountOverflow)?;
            let copy_samples = copy_frames
                .checked_mul(channels)
                .ok_or(SourceAudioError::SampleCountOverflow)?;
            let source_end = source_start
                .checked_add(copy_samples)
                .ok_or(SourceAudioError::SampleCountOverflow)?;
            let output_end = output_offset
                .checked_add(copy_samples)
                .ok_or(SourceAudioError::SampleCountOverflow)?;
            output[output_offset..output_end]
                .copy_from_slice(&window.samples()[source_start..source_end]);
            output_offset = output_end;
            frame = copy_end;
        }
        Ok(())
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.windows.is_empty()
    }

    fn window_at(&self, frame: u64) -> Option<&SourceAudioWindow> {
        self.windows
            .iter()
            .find(|window| window.range().contains_frame(frame))
    }
}

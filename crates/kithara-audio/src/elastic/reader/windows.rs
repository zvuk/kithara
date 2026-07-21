use std::mem::replace;

use kithara_bufpool::PcmBuf;
use kithara_events::PlaybackDirection;
use num_traits::ToPrimitive;

use super::{Active, BufferedSourceWindow, ElasticReader, PendingSourceRead};
use crate::{
    PcmReader, SourceFrameIndex, SourceRange, SourceRangeError, SourceRangeReadOutcome,
    elastic::ElasticReaderError,
};

impl<State> ElasticReader<State> {
    pub(super) fn next_source_window(
        &self,
        current: SourceRange,
        direction: PlaybackDirection,
    ) -> Result<Option<SourceRange>, SourceRangeError> {
        let overlap = SourceFrameIndex::try_from(self.max_source_frames)?.get();
        let (start, end) = match direction {
            PlaybackDirection::Forward => {
                let start = current.end().get().saturating_sub(overlap);
                let end = start
                    .saturating_add(self.source_window_frames)
                    .min(self.source_frame_count);
                (start, end)
            }
            PlaybackDirection::Reverse => {
                let end = current
                    .start()
                    .get()
                    .saturating_add(overlap)
                    .min(self.source_frame_count);
                let start = end.saturating_sub(self.source_window_frames);
                (start, end)
            }
        };
        let advances = match direction {
            PlaybackDirection::Forward => end > current.end().get(),
            PlaybackDirection::Reverse => start < current.start().get(),
        };
        if !advances || start >= end {
            return Ok(None);
        }
        SourceRange::try_from(start..end).map(Some)
    }
}

impl ElasticReader<Active> {
    #[must_use]
    pub fn decoded_frontier(&self) -> f64 {
        let active = self.state.runtime.prepared.source_window.end().get();
        let frame = match self.state.runtime.prepared.direction {
            PlaybackDirection::Forward => self
                .ready_windows
                .back()
                .map_or(active, |window| window.range.end().get()),
            PlaybackDirection::Reverse => active,
        };
        frame
            .to_f64()
            .map_or(0.0, |frame| frame / f64::from(self.sample_rate.get()))
    }

    pub(super) fn ensure_window<R>(
        &mut self,
        source: &mut R,
        range: SourceRange,
        direction: PlaybackDirection,
    ) -> Result<(), ElasticReaderError>
    where
        R: PcmReader + ?Sized,
    {
        self.poll_source_read(source, direction)?;
        let source_window = self.state.runtime.prepared.source_window;
        if source_window.start() <= range.start() && range.end() <= source_window.end() {
            self.schedule_window(source, direction)?;
            return Ok(());
        }
        if self
            .ready_windows
            .front()
            .map(|window| window.range)
            .is_some_and(|window| window.start() <= range.start() && range.end() <= window.end())
        {
            let window = self
                .ready_windows
                .pop_front()
                .ok_or(ElasticReaderError::SourceUnavailable)?;
            let old = replace(&mut self.fetch, window.samples);
            self.window_buffers.push(old);
            self.state.runtime.prepared.source_window = window.range;
            self.schedule_window(source, direction)?;
            return Ok(());
        }
        self.schedule_window(source, direction)?;
        Err(ElasticReaderError::SourceWindowDeadlineMissed)
    }

    pub(super) fn poll_source_read<R>(
        &mut self,
        source: &mut R,
        direction: PlaybackDirection,
    ) -> Result<(), ElasticReaderError>
    where
        R: PcmReader + ?Sized,
    {
        let Some(pending) = self.state.runtime.pending_source_read.as_mut() else {
            return Ok(());
        };
        let range = pending.request.range();
        let frames = usize::try_from(range.len()).map_err(|_| ElasticReaderError::FrameOverflow)?;
        let sample_len = frames
            .checked_mul(self.backend.capabilities().channels())
            .ok_or(ElasticReaderError::FrameOverflow)?;
        let outcome = source.read_source_range(pending.request, &mut pending.samples[..sample_len]);
        match outcome {
            Ok(SourceRangeReadOutcome::Pending) => Ok(()),
            Ok(SourceRangeReadOutcome::Ready { .. }) => {
                let pending = self
                    .state
                    .runtime
                    .pending_source_read
                    .take()
                    .ok_or(ElasticReaderError::SourceUnavailable)?;
                self.stage_ready_window(pending.request.range(), pending.samples, direction)
            }
            Ok(SourceRangeReadOutcome::Eof) => {
                self.release_pending_source_read();
                Err(ElasticReaderError::SourceReadFailed)
            }
            Err(error) => {
                self.release_pending_source_read();
                Err(error.into())
            }
        }
    }

    pub(super) fn release_pending_source_read(&mut self) {
        if let Some(pending) = self.state.runtime.pending_source_read.take() {
            self.window_buffers.push(pending.samples);
        }
    }

    pub(super) fn schedule_window<R>(
        &mut self,
        source: &mut R,
        direction: PlaybackDirection,
    ) -> Result<(), ElasticReaderError>
    where
        R: PcmReader + ?Sized,
    {
        if self.state.runtime.relocation.is_some() {
            return Ok(());
        }
        if self.ready_windows.len() >= self.config.ready_window_count() {
            return Ok(());
        }
        if self.state.runtime.pending_source_read.is_some() || self.window_buffers.is_empty() {
            return Ok(());
        }
        let current = self
            .ready_windows
            .back()
            .map_or(self.state.runtime.prepared.source_window, |window| {
                window.range
            });
        let Some(request_range) = self.next_source_window(current, direction)? else {
            return Ok(());
        };
        let request = source.request_source_range(request_range)?;
        let samples = self
            .window_buffers
            .pop()
            .ok_or(ElasticReaderError::SourceUnavailable)?;
        self.state.runtime.pending_source_read = Some(PendingSourceRead { samples, request });
        Ok(())
    }

    pub(super) fn stage_ready_window(
        &mut self,
        range: SourceRange,
        samples: PcmBuf,
        direction: PlaybackDirection,
    ) -> Result<(), ElasticReaderError> {
        let frontier = self
            .ready_windows
            .back()
            .map_or(self.state.runtime.prepared.source_window, |window| {
                window.range
            });
        let advances = match direction {
            PlaybackDirection::Forward => range.end() > frontier.end(),
            PlaybackDirection::Reverse => range.start() < frontier.start(),
        };
        if !advances || self.ready_windows.len() >= self.config.ready_window_count() {
            self.window_buffers.push(samples);
            return Err(ElasticReaderError::SourceReadFailed);
        }
        self.ready_windows
            .push_back(BufferedSourceWindow { samples, range });
        Ok(())
    }
}

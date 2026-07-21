use std::mem;

use kithara_bufpool::PcmBuf;
use kithara_events::PlaybackDirection;

use super::{
    super::{ElasticAnchor, ElasticReaderError},
    Active, ElasticReader, ElasticRelocation, PendingSourceRead, RelocationRead, sample_count,
};
use crate::{PcmReader, SourceRangeReadOutcome};

impl ElasticReader<Active> {
    /// Stages a relocation without changing the audible cursor.
    /// # Errors
    /// Returns a typed direction, range, or pending-relocation error.
    pub fn begin_relocation(&mut self, anchor: ElasticAnchor) -> Result<(), ElasticReaderError> {
        if anchor.direction() == PlaybackDirection::Reverse
            && !self.backend.capabilities().supports_reverse()
        {
            return Err(ElasticReaderError::ReverseUnsupported);
        }
        if self.state.runtime.relocation.is_some() {
            return Err(ElasticReaderError::RelocationPending);
        }
        let preparation =
            self.plan_preparation(anchor, self.config.relocation_prefetch_blocks())?;
        self.state.runtime.relocation = Some(ElasticRelocation {
            preparation,
            read: RelocationRead::Idle,
        });
        Ok(())
    }

    fn begin_relocation_read<R>(&mut self, source: &mut R) -> Result<(), ElasticReaderError>
    where
        R: PcmReader + ?Sized,
    {
        let range = self
            .state
            .runtime
            .relocation
            .as_ref()
            .ok_or(ElasticReaderError::RelocationNotReady)?
            .preparation
            .fetch_range;
        self.release_pending_source_read();
        let request = source.request_source_range(range)?;
        let samples = self
            .relocation_buffer
            .take()
            .ok_or(ElasticReaderError::SourceUnavailable)?;
        self.state
            .runtime
            .relocation
            .as_mut()
            .ok_or(ElasticReaderError::RelocationNotReady)?
            .read = RelocationRead::Pending(PendingSourceRead { samples, request });
        Ok(())
    }

    /// Publishes a fully read relocation and re-primes the DSP backend.
    /// # Errors
    /// Returns a typed readiness, range, buffer, or DSP error.
    pub fn commit_relocation(&mut self) -> Result<(), ElasticReaderError> {
        let relocation = self
            .state
            .runtime
            .relocation
            .as_ref()
            .ok_or(ElasticReaderError::RelocationNotReady)?;
        let range = relocation.preparation.fetch_range;
        let fetch_frames =
            usize::try_from(range.len()).map_err(|_| ElasticReaderError::FrameOverflow)?;
        if fetch_frames > self.max_fetch_frames {
            return Err(ElasticReaderError::FetchWindowMismatch);
        }
        let fetch_samples = sample_count(fetch_frames, self.backend.capabilities().channels())
            .map_err(|_| ElasticReaderError::FrameOverflow)?;
        let Some(mut relocation) = self.state.runtime.relocation.take() else {
            return Err(ElasticReaderError::RelocationNotReady);
        };
        let samples = match mem::replace(&mut relocation.read, RelocationRead::Idle) {
            RelocationRead::Ready(samples) => samples,
            read => {
                relocation.read = read;
                self.state.runtime.relocation = Some(relocation);
                return Err(ElasticReaderError::RelocationNotReady);
            }
        };
        self.release_pending_source_read();
        while let Some(window) = self.ready_windows.pop_back() {
            self.window_buffers.push(window.samples);
        }
        let old = mem::replace(&mut self.fetch, samples);
        debug_assert!(self.relocation_buffer.is_none());
        self.relocation_buffer = Some(old);
        self.prime(relocation.preparation, fetch_samples)
            .map_err(|_| ElasticReaderError::RelocationPreparationFailed)?;
        self.state.runtime.prepared.cursor = relocation.preparation.anchor;
        self.state.runtime.prepared.direction = relocation.preparation.direction;
        self.state.runtime.prepared.source_window = range;
        Ok(())
    }

    pub fn discard_relocation(&mut self) {
        let Some(mut relocation) = self.state.runtime.relocation.take() else {
            return;
        };
        self.restore_relocation_buffer(relocation.read.take_samples());
    }

    /// Polls relocation source work and reports whether it is ready to commit.
    /// # Errors
    /// Returns a typed source, range, or relocation-state error.
    pub fn poll_relocation<R>(&mut self, source: &mut R) -> Result<bool, ElasticReaderError>
    where
        R: PcmReader + ?Sized,
    {
        self.poll_relocation_read(source)?;
        let relocation = self
            .state
            .runtime
            .relocation
            .as_ref()
            .ok_or(ElasticReaderError::RelocationNotReady)?;
        match relocation.read {
            RelocationRead::Ready(_) => Ok(true),
            RelocationRead::Pending(_) => Ok(false),
            RelocationRead::Idle => {
                self.begin_relocation_read(source)?;
                Ok(false)
            }
        }
    }

    pub(super) fn poll_relocation_read<R>(
        &mut self,
        source: &mut R,
    ) -> Result<(), ElasticReaderError>
    where
        R: PcmReader + ?Sized,
    {
        let outcome = {
            let Some(relocation) = self.state.runtime.relocation.as_mut() else {
                return Ok(());
            };
            let RelocationRead::Pending(pending) = &mut relocation.read else {
                return Ok(());
            };
            let frames = usize::try_from(pending.request.range().len())
                .map_err(|_| ElasticReaderError::FrameOverflow)?;
            let sample_len = frames
                .checked_mul(self.backend.capabilities().channels())
                .ok_or(ElasticReaderError::FrameOverflow)?;
            source.read_source_range(pending.request, &mut pending.samples[..sample_len])
        };
        match outcome {
            Ok(SourceRangeReadOutcome::Pending) => Ok(()),
            Ok(SourceRangeReadOutcome::Ready { .. }) => {
                let relocation = self
                    .state
                    .runtime
                    .relocation
                    .as_mut()
                    .ok_or(ElasticReaderError::RelocationNotReady)?;
                let RelocationRead::Pending(pending) =
                    mem::replace(&mut relocation.read, RelocationRead::Idle)
                else {
                    return Err(ElasticReaderError::RelocationNotReady);
                };
                relocation.read = RelocationRead::Ready(pending.samples);
                Ok(())
            }
            Ok(SourceRangeReadOutcome::Eof) => {
                self.release_relocation_read();
                Err(ElasticReaderError::SourceReadFailed)
            }
            Err(error) => {
                self.release_relocation_read();
                Err(error.into())
            }
        }
    }

    /// Polls an already-started relocation read without starting source work.
    /// # Errors
    /// Returns a typed source, range, or relocation-state error.
    pub fn refresh_relocation<R>(&mut self, source: &mut R) -> Result<(), ElasticReaderError>
    where
        R: PcmReader + ?Sized,
    {
        self.poll_relocation_read(source)
    }

    fn release_relocation_read(&mut self) {
        let samples = self
            .state
            .runtime
            .relocation
            .as_mut()
            .and_then(|relocation| relocation.read.take_samples());
        self.restore_relocation_buffer(samples);
    }

    fn restore_relocation_buffer(&mut self, samples: Option<PcmBuf>) {
        if let Some(samples) = samples {
            debug_assert!(self.relocation_buffer.is_none());
            self.relocation_buffer = Some(samples);
        }
    }
}

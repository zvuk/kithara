use std::mem;

use super::{
    Active, ElasticPrepareError, ElasticRelocation, ElasticRenderError, ElasticRenderer,
    PendingSourceRead, PlaybackDirection, RelocationRead, SessionBeat, SourceRangeReadOutcome,
    Tempo, TrackBinding, TransportRevision, sample_count,
};
use crate::resource::Resource;

impl ElasticRenderer<Active> {
    pub(crate) fn begin_relocation(
        &mut self,
        binding: &TrackBinding,
        target: SessionBeat,
        tempo: Tempo,
        revision: TransportRevision,
    ) -> Result<(), ElasticPrepareError> {
        if binding.direction() == PlaybackDirection::Reverse
            && !self.backend.capabilities().supports_reverse()
        {
            return Err(ElasticPrepareError::ReverseUnsupported);
        }
        if self.state.runtime.relocation.is_some() {
            return Err(ElasticPrepareError::RelocationPending);
        }
        let preparation =
            self.plan_preparation(binding, target, tempo, Self::RELOCATION_PREFETCH_BLOCKS)?;
        self.state.runtime.relocation = Some(ElasticRelocation {
            preparation,
            revision,
            target,
            read: RelocationRead::Idle,
        });
        Ok(())
    }

    fn begin_relocation_read(&mut self, source: &mut Resource) -> Result<(), ElasticRenderError> {
        let range = self
            .state
            .runtime
            .relocation
            .as_ref()
            .ok_or(ElasticRenderError::RelocationNotReady)?
            .preparation
            .fetch_range;
        self.release_pending_source_read();
        let request = source.request_source_range(range)?;
        let samples = self
            .relocation_buffer
            .take()
            .ok_or(ElasticRenderError::SourceUnavailable)?;
        self.state
            .runtime
            .relocation
            .as_mut()
            .ok_or(ElasticRenderError::RelocationNotReady)?
            .read = RelocationRead::Pending(PendingSourceRead { samples, request });
        Ok(())
    }

    pub(super) fn commit_relocation(
        &mut self,
        revision: TransportRevision,
    ) -> Result<(), ElasticRenderError> {
        let relocation = self
            .state
            .runtime
            .relocation
            .as_ref()
            .ok_or(ElasticRenderError::RelocationNotReady)?;
        if relocation.revision != revision {
            return Err(ElasticRenderError::RevisionMismatch);
        }
        let range = relocation.preparation.fetch_range;
        let fetch_frames =
            usize::try_from(range.len()).map_err(|_| ElasticRenderError::FrameOverflow)?;
        if fetch_frames > self.max_fetch_frames {
            return Err(ElasticRenderError::FetchWindowMismatch);
        }
        let fetch_samples = sample_count(fetch_frames, self.backend.capabilities().channels())
            .map_err(|_| ElasticRenderError::FrameOverflow)?;
        let Some(mut relocation) = self.state.runtime.relocation.take() else {
            return Err(ElasticRenderError::RelocationNotReady);
        };
        let samples = match mem::replace(&mut relocation.read, RelocationRead::Idle) {
            RelocationRead::Ready(samples) => samples,
            read => {
                relocation.read = read;
                self.state.runtime.relocation = Some(relocation);
                return Err(ElasticRenderError::RelocationNotReady);
            }
        };
        self.release_pending_source_read();
        while let Some(window) = self.ready_windows.pop() {
            self.window_buffers.push(window.samples);
        }
        let old = mem::replace(&mut self.fetch, samples);
        debug_assert!(self.relocation_buffer.is_none());
        self.relocation_buffer = Some(old);
        self.prime(relocation.preparation, fetch_samples)
            .map_err(|_| ElasticRenderError::RelocationPreparationFailed)?;
        self.state.runtime.prepared.cursor = relocation.preparation.anchor;
        self.state.runtime.prepared.direction = relocation.preparation.direction;
        self.state.runtime.prepared.source_window = range;
        self.state.runtime.prepared.revision = revision;
        Ok(())
    }

    pub(crate) fn discard_relocation(&mut self, revision: TransportRevision) {
        if self
            .state
            .runtime
            .relocation
            .as_ref()
            .is_none_or(|relocation| relocation.revision != revision)
        {
            return;
        }
        let Some(mut relocation) = self.state.runtime.relocation.take() else {
            return;
        };
        let samples = match mem::replace(&mut relocation.read, RelocationRead::Idle) {
            RelocationRead::Pending(pending) => Some(pending.samples),
            RelocationRead::Ready(samples) => Some(samples),
            RelocationRead::Idle => None,
        };
        if let Some(samples) = samples {
            debug_assert!(self.relocation_buffer.is_none());
            self.relocation_buffer = Some(samples);
        }
    }

    pub(crate) fn poll_relocation(
        &mut self,
        source: &mut Resource,
        revision: TransportRevision,
    ) -> Result<bool, ElasticRenderError> {
        self.poll_relocation_read(source)?;
        let relocation = self
            .state
            .runtime
            .relocation
            .as_ref()
            .ok_or(ElasticRenderError::RelocationNotReady)?;
        if relocation.revision != revision {
            return Err(ElasticRenderError::RevisionMismatch);
        }
        match relocation.read {
            RelocationRead::Ready(_) => Ok(true),
            RelocationRead::Pending(_) => Ok(false),
            RelocationRead::Idle => {
                self.begin_relocation_read(source)?;
                Ok(false)
            }
        }
    }

    pub(super) fn poll_relocation_read(
        &mut self,
        source: &mut Resource,
    ) -> Result<(), ElasticRenderError> {
        let outcome = {
            let Some(relocation) = self.state.runtime.relocation.as_mut() else {
                return Ok(());
            };
            let RelocationRead::Pending(pending) = &mut relocation.read else {
                return Ok(());
            };
            let frames = usize::try_from(pending.request.range().len())
                .map_err(|_| ElasticRenderError::FrameOverflow)?;
            let sample_len = frames
                .checked_mul(self.backend.capabilities().channels())
                .ok_or(ElasticRenderError::FrameOverflow)?;
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
                    .ok_or(ElasticRenderError::RelocationNotReady)?;
                let RelocationRead::Pending(pending) =
                    mem::replace(&mut relocation.read, RelocationRead::Idle)
                else {
                    return Err(ElasticRenderError::RelocationNotReady);
                };
                relocation.read = RelocationRead::Ready(pending.samples);
                Ok(())
            }
            Ok(SourceRangeReadOutcome::Eof) => {
                self.release_relocation_read();
                Err(ElasticRenderError::SourceReadFailed)
            }
            Err(error) => {
                self.release_relocation_read();
                Err(error.into())
            }
        }
    }

    fn release_relocation_read(&mut self) {
        let Some(relocation) = self.state.runtime.relocation.as_mut() else {
            return;
        };
        let samples = match mem::replace(&mut relocation.read, RelocationRead::Idle) {
            RelocationRead::Pending(pending) => Some(pending.samples),
            RelocationRead::Ready(samples) => Some(samples),
            RelocationRead::Idle => None,
        };
        if let Some(samples) = samples {
            debug_assert!(self.relocation_buffer.is_none());
            self.relocation_buffer = Some(samples);
        }
    }
}

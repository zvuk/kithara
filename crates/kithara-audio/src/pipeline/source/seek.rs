use kithara_decode::{DecodeError, DecodeResult, DecoderSeekOutcome};
use kithara_platform::time::Duration;
use kithara_stream::{PendingReason, SourcePhase, StreamType};

use super::{
    change::SourceCommand,
    core::{OwnerPhase, StreamAudioSource},
};
use crate::{
    AudioEvent, SeekOutcome, TrackFailureKind,
    pipeline::{
        decode::format::{FormatDecision, detect},
        rebuild::{RecreateCause, RecreateState},
        seek::{ResumeTarget, emit::commit_outcome},
    },
};

impl<T: StreamType> StreamAudioSource<T> {
    pub(super) fn release_seek_abr(&mut self) {
        if let Some(abr) = self.seek_abr.take() {
            abr.unlock();
        }
    }

    pub(super) fn resume_target(&self) -> ResumeTarget {
        if let OwnerPhase::Changing(change) = &self.phase
            && let Some(target) = change.command.target()
        {
            return target;
        }
        if let OwnerPhase::Seeking { target, .. } = self.phase {
            return target;
        }
        if let Some(target) = self.decode.active().resume_target() {
            return target;
        }
        self.resume.source_end().map_or_else(
            || ResumeTarget::Position(self.playhead.position()),
            ResumeTarget::Source,
        )
    }

    pub(super) fn prepare_active(&mut self) -> Result<(), (TrackFailureKind, DecodeError)> {
        if let OwnerPhase::Changing(change) = &self.phase {
            let offset = match &change.command {
                SourceCommand::Recreate { recreate, .. } => recreate.offset,
                SourceCommand::Seek(_) | SourceCommand::Rate { .. } => {
                    self.decode.active().base_offset()
                }
            };
            self.prepare_source_change(offset)?;
            if !matches!(
                self.phase,
                OwnerPhase::Decoding | OwnerPhase::Seeking { .. }
            ) {
                return Ok(());
            }
        }
        loop {
            let seeking = match self.phase {
                OwnerPhase::Seeking { target, .. } => Some(target),
                _ => None,
            };
            if let OwnerPhase::Seeking {
                target,
                reason: PendingReason::SessionRetired,
            } = self.phase
            {
                self.replace_retired_reader(target)?;
                return Ok(());
            }
            let Some(outcome) = self.prepare_decoder_seek(seeking.is_some())? else {
                return Ok(());
            };
            if !self.continue_prepared_seek(outcome, seeking)? {
                return Ok(());
            }
        }
    }

    fn continue_prepared_seek(
        &mut self,
        outcome: DecoderSeekOutcome,
        seeking: Option<ResumeTarget>,
    ) -> Result<bool, (TrackFailureKind, DecodeError)> {
        let target = self.resume_target();
        if matches!(
            outcome,
            DecoderSeekOutcome::Pending(PendingReason::SessionRetired)
        ) {
            self.replace_retired_reader(target)?;
            return Ok(false);
        }
        if matches!(
            outcome,
            DecoderSeekOutcome::Pending(PendingReason::VariantChange)
        ) && let FormatDecision::Recreate(recreate) =
            detect(&self.shared_stream, self.decode.active())
        {
            let offset = recreate.offset;
            self.install_replacement(recreate, Some(target))
                .map_err(|error| (TrackFailureKind::RecreateFailed { offset }, error))?;
            return Ok(false);
        }
        if let Some(target) = seeking {
            self.accept_seek_outcome(target, outcome).map_err(|error| {
                (
                    TrackFailureKind::Decode {
                        kind: crate::map_decode_error_kind(&error),
                    },
                    error,
                )
            })?;
        }
        Ok(matches!(outcome, DecoderSeekOutcome::Landed { .. }))
    }

    fn prepare_source_change(
        &mut self,
        offset: u64,
    ) -> Result<(), (TrackFailureKind, DecodeError)> {
        self.drive_source_change().map(|_| ()).map_err(|error| {
            let failure = if self.shared_stream.phase() == SourcePhase::Cancelled {
                TrackFailureKind::SourceCancelled
            } else {
                TrackFailureKind::RecreateFailed { offset }
            };
            (failure, error)
        })
    }

    fn prepare_decoder_seek(
        &mut self,
        seeking: bool,
    ) -> Result<Option<DecoderSeekOutcome>, (TrackFailureKind, DecodeError)> {
        let outcome = self.decode.prepare_deferred(true).map_err(|error| {
            let failure = if self.shared_stream.phase() == SourcePhase::Cancelled {
                TrackFailureKind::SourceCancelled
            } else {
                TrackFailureKind::Decode {
                    kind: crate::map_decode_error_kind(&error),
                }
            };
            (failure, error)
        })?;
        if self.shared_stream.phase() == SourcePhase::Cancelled {
            return Err((TrackFailureKind::SourceCancelled, DecodeError::Interrupted));
        }
        if outcome.is_none() {
            return if seeking {
                Err((
                    TrackFailureKind::Decode {
                        kind: crate::DecodeErrorKind::InvalidData,
                    },
                    DecodeError::InvalidData {
                        detail: "pending seek has no decoder continuation",
                    },
                ))
            } else {
                Ok(None)
            };
        };
        Ok(outcome)
    }

    pub(super) fn replace_retired_reader(
        &mut self,
        target: ResumeTarget,
    ) -> Result<DecoderSeekOutcome, (TrackFailureKind, DecodeError)> {
        self.install_replacement(
            RecreateState {
                media_info: None,
                offset: self.decode.active().base_offset(),
                cause: RecreateCause::VariantSwitch,
            },
            Some(target),
        )
        .map_err(|error| {
            let failure = if self.shared_stream.phase() == SourcePhase::Cancelled {
                TrackFailureKind::SourceCancelled
            } else {
                TrackFailureKind::RecreateFailed {
                    offset: self.decode.active().base_offset(),
                }
            };
            (failure, error)
        })?;
        Ok(DecoderSeekOutcome::Pending(PendingReason::Retry))
    }

    pub(super) fn accept_seek_outcome(
        &mut self,
        target: ResumeTarget,
        outcome: DecoderSeekOutcome,
    ) -> DecodeResult<SeekOutcome> {
        if self.shared_stream.phase() == SourcePhase::Cancelled {
            return Err(DecodeError::Interrupted);
        }
        let position = DecodeResult::from(target)?;
        let accepted = match outcome {
            DecoderSeekOutcome::Pending(reason) => {
                self.phase = OwnerPhase::Seeking { target, reason };
                self.waiting_for_source = reason != PendingReason::Retry;
                return Ok(SeekOutcome::Pending {
                    target: position,
                    reason,
                });
            }
            DecoderSeekOutcome::Landed { landed_at, .. } => {
                commit_outcome(
                    self.decode.active(),
                    &self.shared_stream,
                    self.playhead.as_ref(),
                    &outcome,
                )?;
                self.phase = OwnerPhase::Decoding;
                SeekOutcome::Landed {
                    target: position,
                    landed_at,
                }
            }
            DecoderSeekOutcome::PastEof { duration } => {
                commit_outcome(
                    self.decode.active(),
                    &self.shared_stream,
                    self.playhead.as_ref(),
                    &outcome,
                )?;
                self.phase = OwnerPhase::AtEof;
                self.release_seek_abr();
                SeekOutcome::PastEof {
                    target: position,
                    duration,
                }
            }
        };
        self.waiting_for_source = false;
        self.emit.enqueue(AudioEvent::SeekLifecycle {
            stage: crate::SeekLifecycleStage::SeekApplied,
            location: crate::SegmentLocation::default(),
        });
        Ok(accepted)
    }

    pub(super) fn seek_selected(
        &mut self,
        position: Duration,
        for_render: bool,
    ) -> Result<SeekOutcome, crate::AudioReadError> {
        if self.shared_stream.phase() == SourcePhase::Cancelled {
            self.fail(TrackFailureKind::SourceCancelled, None);
        }
        if let OwnerPhase::Failed { failure, .. } = self.phase {
            return Err(crate::AudioReadError::Stream {
                what: "seek decoded source",
                source: crate::FailureSource::ProducerAfterSeek { failure },
            });
        }
        self.emit.enqueue(AudioEvent::SeekLifecycle {
            stage: crate::SeekLifecycleStage::SeekRequest,
            location: crate::SegmentLocation::default(),
        });
        let result = self.seek_owned(position, for_render);
        if let Err(error) = &result {
            let failure = if self.shared_stream.phase() == SourcePhase::Cancelled {
                TrackFailureKind::SourceCancelled
            } else {
                TrackFailureKind::from(error)
            };
            let rejected_by_failure = matches!(self.pending_seek_target(),
                Some(ResumeTarget::Position(target) | ResumeTarget::Render(target)) if target == position);
            self.fail(failure, None);
            if !rejected_by_failure {
                self.emit
                    .enqueue(AudioEvent::SeekRejected { target: position });
            }
        }
        result
    }

    pub(super) fn seek_owned(
        &mut self,
        position: Duration,
        for_render: bool,
    ) -> Result<SeekOutcome, crate::AudioReadError> {
        if !position.is_zero() {
            self.decode.seal_visible_origin();
        }
        if self.seek_abr.is_none()
            && let Some(abr) = self.shared_stream.abr_handle()
        {
            abr.lock();
            self.seek_abr = Some(abr);
        }
        self.abandon_incoming();
        self.resume.clear();
        self.waiting_for_source = false;
        let target = if for_render {
            ResumeTarget::Render(position)
        } else {
            ResumeTarget::Position(position)
        };
        let result = self.request_source_change(SourceCommand::Seek(target))?;
        Ok(result.unwrap_or(SeekOutcome::Pending {
            target: position,
            reason: PendingReason::Retry,
        }))
    }
}

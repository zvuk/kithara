use std::io::SeekFrom;

use kithara_decode::{DecodeError, DecodeResult, DecoderSeekOutcome, InputState};
use kithara_platform::{sync::mpsc, tokio::task::spawn_blocking_on};
use kithara_stream::{PendingReason, ReaderControl, SourcePhase, StreamType};

use super::core::{OwnerPhase, StreamAudioSource};
use crate::{
    AudioEvent, DecoderChangeCause, SeekOutcome,
    pipeline::{
        decode::{
            DecoderGeneration,
            format::{FormatDecision, detect},
        },
        rebuild::{RecreateCause, RecreateState},
        seek::ResumeTarget,
    },
};

pub(super) enum SourceCommand {
    Seek(ResumeTarget),
    Rate {
        target: ResumeTarget,
        apply_seek: bool,
    },
    Recreate {
        recreate: RecreateState,
        landing: Option<ResumeTarget>,
        apply_seek: bool,
    },
}

impl SourceCommand {
    pub(super) fn target(&self) -> Option<ResumeTarget> {
        match self {
            Self::Seek(target) | Self::Rate { target, .. } => Some(*target),
            Self::Recreate { landing, .. } => *landing,
        }
    }

    pub(super) fn applies_seek(&self) -> bool {
        match self {
            Self::Seek(_) => true,
            Self::Rate { apply_seek, .. } | Self::Recreate { apply_seek, .. } => *apply_seek,
        }
    }
}

type Replacement = DecodeResult<(DecoderGeneration, Option<DecoderSeekOutcome>, RecreateCause)>;

enum ChangeWork {
    Releasing,
    ReleasingReplacement,
    Building {
        completion: mpsc::Receiver<Replacement>,
        control: ReaderControl,
    },
    Retiring {
        completion: mpsc::Receiver<Replacement>,
        control: ReaderControl,
    },
}

pub(super) struct SourceChange {
    pub(super) command: SourceCommand,
    work: ChangeWork,
}

impl<T: StreamType> StreamAudioSource<T> {
    pub(super) fn cancel_source_change(&self) {
        if let OwnerPhase::Changing(SourceChange {
            work: ChangeWork::Building { control, .. } | ChangeWork::Retiring { control, .. },
            ..
        }) = &self.phase
        {
            control.cancel();
        }
    }

    pub(super) fn request_source_change(
        &mut self,
        command: SourceCommand,
    ) -> DecodeResult<Option<SeekOutcome>> {
        let work = match std::mem::replace(&mut self.phase, OwnerPhase::Decoding) {
            OwnerPhase::Changing(SourceChange { work, .. }) => match work {
                ChangeWork::Building {
                    completion,
                    control,
                }
                | ChangeWork::Retiring {
                    completion,
                    control,
                } => {
                    control.cancel();
                    ChangeWork::Retiring {
                        completion,
                        control,
                    }
                }
                ChangeWork::Releasing => ChangeWork::Releasing,
                ChangeWork::ReleasingReplacement => ChangeWork::ReleasingReplacement,
            },
            _ => {
                if matches!(command, SourceCommand::Seek(_)) {
                    ChangeWork::Releasing
                } else {
                    ChangeWork::ReleasingReplacement
                }
            }
        };
        self.phase = OwnerPhase::Changing(SourceChange { command, work });
        self.drive_source_change()
    }

    pub(super) fn drive_source_change(&mut self) -> DecodeResult<Option<SeekOutcome>> {
        let OwnerPhase::Changing(mut change) =
            std::mem::replace(&mut self.phase, OwnerPhase::Decoding)
        else {
            return Err(DecodeError::InvalidData {
                detail: "source change has no retained operation",
            });
        };
        let seek_target = change
            .command
            .target()
            .filter(|_| change.command.applies_seek());
        let result = (|| {
            if self.shared_stream.phase() == SourcePhase::Cancelled {
                if let ChangeWork::Building { control, .. } | ChangeWork::Retiring { control, .. } =
                    &change.work
                {
                    control.cancel();
                }
                return Err(DecodeError::Interrupted);
            }
            match change.work {
                ChangeWork::Releasing | ChangeWork::ReleasingReplacement => {
                    let reuse_idle = matches!(change.work, ChangeWork::Releasing);
                    match self.decode.release_input() {
                        InputState::Busy | InputState::Retiring => {
                            self.phase = OwnerPhase::Changing(change);
                            return Ok(None);
                        }
                        InputState::Idle
                            if reuse_idle && matches!(change.command, SourceCommand::Seek(_)) =>
                        {
                            let SourceCommand::Seek(target) = change.command else {
                                unreachable!()
                            };
                            return self.seek_released_input(target).map(Some);
                        }
                        InputState::Idle | InputState::Retired => {}
                    }
                    let (completion, control) = self.start_source_replacement(&change.command);
                    change.work = ChangeWork::Building {
                        completion,
                        control,
                    };
                    self.phase = OwnerPhase::Changing(change);
                    Ok(None)
                }
                ChangeWork::Retiring {
                    completion,
                    control,
                } => match completion.try_recv() {
                    Ok(result) => {
                        drop(result);
                        change.work = ChangeWork::ReleasingReplacement;
                        self.phase = OwnerPhase::Changing(change);
                        self.drive_source_change()
                    }
                    Err(mpsc::TryRecvError::Empty) => {
                        change.work = ChangeWork::Retiring {
                            completion,
                            control,
                        };
                        self.phase = OwnerPhase::Changing(change);
                        Ok(None)
                    }
                    Err(_) => Err(DecodeError::InvalidData {
                        detail: "retiring source construction channel closed",
                    }),
                },
                ChangeWork::Building {
                    completion,
                    control,
                } => {
                    let result = match completion.try_recv() {
                        Ok(result) => result,
                        Err(mpsc::TryRecvError::Empty) => {
                            change.work = ChangeWork::Building {
                                completion,
                                control,
                            };
                            self.phase = OwnerPhase::Changing(change);
                            return Ok(None);
                        }
                        Err(_) => {
                            return Err(DecodeError::InvalidData {
                                detail: "source construction channel closed",
                            });
                        }
                    };
                    let (generation, outcome, cause) = result?;
                    if control.is_cancelled() {
                        drop(generation);
                        return Err(DecodeError::Interrupted);
                    }
                    self.commit_source_replacement(generation, cause, change.command.target())?;
                    if change.command.applies_seek() {
                        let target = change.command.target().ok_or(DecodeError::InvalidData {
                            detail: "source seek replacement lost its target",
                        })?;
                        let outcome = outcome.ok_or(DecodeError::InvalidData {
                            detail: "source seek replacement has no settlement",
                        })?;
                        self.accept_seek_outcome(target, outcome).map(Some)
                    } else {
                        self.phase = if self.decode.active().is_finished() {
                            OwnerPhase::AtEof
                        } else {
                            OwnerPhase::Decoding
                        };
                        Ok(None)
                    }
                }
            }
        })();
        if result.is_err()
            && !matches!(self.phase, OwnerPhase::Failed { .. })
            && let Some(target) = seek_target
        {
            self.phase = OwnerPhase::Seeking {
                target,
                reason: PendingReason::Retry,
            };
        }
        result
    }

    fn seek_released_input(&mut self, target: ResumeTarget) -> DecodeResult<SeekOutcome> {
        let position = DecodeResult::from(target)?;
        if let Some(duration) = self
            .decode
            .active()
            .visible_origin()
            .and_then(|origin| origin.checked_add(self.playhead.duration()?))
            .filter(|endpoint| position >= *endpoint)
        {
            self.decode.notify_seek();
            return self.accept_seek_outcome(target, DecoderSeekOutcome::PastEof { duration });
        }
        self.shared_stream
            .seek_time_anchor(position)
            .map_err(|source| DecodeError::Io { source })?;
        if let FormatDecision::Recreate(recreate) =
            detect(&self.shared_stream, self.decode.active())
        {
            self.request_source_change(SourceCommand::Recreate {
                recreate,
                landing: Some(target),
                apply_seek: true,
            })?;
            return Ok(SeekOutcome::Pending {
                target: position,
                reason: PendingReason::Retry,
            });
        }
        if let Some(len) = self.shared_stream.len() {
            self.decode
                .update_len(len.saturating_sub(self.decode.active().base_offset()));
        }
        let outcome = match target {
            ResumeTarget::Render(_) => self.decode.seek_for_render(position)?,
            ResumeTarget::Position(_) | ResumeTarget::Source(_) => self.decode.seek(position)?,
        };
        self.accept_seek_outcome(target, outcome)
    }

    fn start_source_replacement(
        &self,
        command: &SourceCommand,
    ) -> (mpsc::Receiver<Replacement>, ReaderControl) {
        let target = command.target();
        let (info, base_offset, cause) = match command {
            SourceCommand::Seek(_) => (
                self.decode.active().media_info().cloned(),
                self.decode.active().base_offset(),
                RecreateCause::VariantSwitch,
            ),
            SourceCommand::Rate { .. } => (
                self.decode.active().media_info().cloned(),
                self.decode.active().base_offset(),
                RecreateCause::HostRateChange,
            ),
            SourceCommand::Recreate { recreate, .. } => {
                (recreate.media_info.clone(), recreate.offset, recreate.cause)
            }
        };
        let shared = self.shared_stream.clone();
        let control = shared.new_reader_control();
        control.set_worker_wake(self.wake.clone());
        let worker_control = control.clone();
        let factory = self.factory.clone();
        let host_rate = self.host_rate;
        let gapless_mode = self.decode.gapless_mode();
        let require_current_media = matches!(
            command,
            SourceCommand::Recreate {
                recreate: RecreateState {
                    media_info: None,
                    cause: RecreateCause::VariantSwitch,
                    ..
                },
                ..
            }
        );
        let wake = self.wake.clone();
        let (completed, completion) = mpsc::channel();
        drop(spawn_blocking_on(&self.runtime, move || {
            let result = (|| {
                if worker_control.is_cancelled() {
                    return Err(DecodeError::Interrupted);
                }
                if let Some(target) = target {
                    shared
                        .seek_time_anchor(DecodeResult::from(target)?)
                        .map_err(|source| DecodeError::Io { source })?;
                }
                if worker_control.is_cancelled() {
                    return Err(DecodeError::Interrupted);
                }
                let offset = if matches!(cause, RecreateCause::VariantSwitch)
                    && shared.variant_control().is_some()
                {
                    shared
                        .format_change_segment_range()
                        .map_err(DecodeError::backend)?
                        .start
                } else {
                    base_offset
                };
                shared
                    .probe_seek(SeekFrom::Start(offset))
                    .map_err(|source| DecodeError::Io { source })?;
                let reader = shared
                    .open_rebuild_reader_with_control(offset, worker_control.clone())
                    .map_err(|source| DecodeError::Io { source })?;
                let info = info.or_else(|| shared.media_info());
                if require_current_media && info.is_none() {
                    return Err(DecodeError::InvalidData {
                        detail: "retired reader has no current media information",
                    });
                }
                factory
                    .build_generation(reader, info, offset, target, host_rate, gapless_mode)
                    .map(|(generation, outcome)| (generation, outcome, cause))
            })();
            let result = if worker_control.is_cancelled() {
                drop(result);
                Err(DecodeError::Interrupted)
            } else {
                result
            };
            drop(completed.send(result));
            wake.wake();
        }));
        (completion, control)
    }

    fn commit_source_replacement(
        &mut self,
        mut generation: DecoderGeneration,
        cause: RecreateCause,
        landing: Option<ResumeTarget>,
    ) -> DecodeResult<()> {
        if landing.is_some() {
            generation.retain_media_origin(self.decode.active().media_origin());
            generation.retain_visible_origin(
                self.decode.active().visible_origin(),
                self.decode.active().comparison_base(),
            );
        } else {
            generation.seal_visible_origin(self.decode.gapless_mode());
        }
        let old_spec = self.decode.output_spec();
        self.decode.prepare_replacement_profile(generation.spec());
        if let Some(error) = self.decode.take_stage_error() {
            return Err(error);
        }
        drop(self.decode.replace_active(generation));
        self.decode.reset();
        self.resume.clear();
        self.waiting_for_source = false;
        if let Some(ResumeTarget::Source(landing)) = landing {
            self.resume.rebase(landing);
        }
        let new_spec = self.decode.output_spec();
        if old_spec != new_spec {
            self.emit.enqueue(AudioEvent::FormatChanged {
                old: old_spec,
                new: new_spec,
            });
        }
        self.publish_generation(match cause {
            RecreateCause::FormatBoundary => DecoderChangeCause::FormatBoundary,
            RecreateCause::HostRateChange => DecoderChangeCause::HostRateChange,
            RecreateCause::VariantSwitch => DecoderChangeCause::VariantSwitch,
        });
        Ok(())
    }
}

use std::num::NonZeroU32;

use kithara_audio::{SessionBeat, SessionFrame};

use crate::api::{SessionTransportSnapshot, Tempo, TransportRevision};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) enum TransportBoundary {
    #[default]
    Continuous,
    Relocate(SessionBeat),
}

#[derive(Clone, Copy, Debug, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(get, vis = "pub(crate)")]
pub(crate) struct SessionTransportCommit {
    #[field(get, copy)]
    tempo: Tempo,
    #[field(get, copy)]
    boundary: TransportBoundary,
    #[field(get, copy)]
    revision: TransportRevision,
    #[field(get = is_playing, copy)]
    playing: bool,
}

impl SessionTransportCommit {
    pub(crate) const fn new(tempo: Tempo, playing: bool, revision: TransportRevision) -> Self {
        Self {
            boundary: TransportBoundary::Continuous,
            tempo,
            playing,
            revision,
        }
    }

    pub(crate) const fn relocate(
        tempo: Tempo,
        playing: bool,
        revision: TransportRevision,
        target: SessionBeat,
    ) -> Self {
        Self {
            boundary: TransportBoundary::Relocate(target),
            tempo,
            playing,
            revision,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(get, vis = "pub(crate)")]
pub(crate) struct TransportCommitStamp {
    #[field(get, copy)]
    sample_rate: NonZeroU32,
    #[field(get, copy)]
    previous: Option<SessionTransportCommit>,
    #[field(get, copy)]
    target_frame: SessionFrame,
    #[field(get, copy)]
    next: SessionTransportCommit,
}

impl TransportCommitStamp {
    pub(crate) const fn new(
        previous: Option<SessionTransportCommit>,
        next: SessionTransportCommit,
        target_frame: SessionFrame,
        sample_rate: NonZeroU32,
    ) -> Self {
        Self {
            sample_rate,
            previous,
            target_frame,
            next,
        }
    }

    delegate::delegate! {
        to self.next {
            pub(crate) fn revision(self) -> TransportRevision;
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(get, vis = "pub(crate)")]
pub(crate) enum TransportCommitResult {
    Aborted(#[field(rename = revision)] TransportRevision),
    Applied(#[field(rename = revision)] TransportRevision),
    Rejected(#[field(rename = revision, copy)] TransportRevision),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(get, vis = "pub(crate)")]
pub(crate) struct TransportObservation {
    #[field(get, copy)]
    completion: Option<TransportCommitResult>,
    #[field(get, copy)]
    snapshot: Option<SessionTransportSnapshot>,
}

impl TransportObservation {
    pub(crate) const fn new(
        completion: Option<TransportCommitResult>,
        snapshot: Option<SessionTransportSnapshot>,
    ) -> Self {
        Self {
            completion,
            snapshot,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum TransportCommitEvent {
    Abort(TransportRevision),
    Apply(TransportRevision),
    Stage(TransportCommitStamp),
}

/// Audio-thread transport failures. The processor logs them through an
/// allocation-free `&'static str`, so `message` is the single source of the
/// text and `Display` forwards to it.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum TransportProcessError {
    #[error("{}", Self::AbortMismatch.message())]
    AbortMismatch,
    #[error("{}", Self::DuplicateEvent.message())]
    DuplicateEvent,
    #[error("{}", Self::FrameDiscontinuity.message())]
    FrameDiscontinuity,
    #[error("{}", Self::InvalidBeatRange.message())]
    InvalidBeatRange,
    #[error("{}", Self::MissingObservation.message())]
    MissingObservation,
    #[error("{}", Self::MissingState.message())]
    MissingState,
    #[error("{}", Self::UnexpectedEvent.message())]
    UnexpectedEvent,
}

impl TransportProcessError {
    pub(crate) const fn message(self) -> &'static str {
        match self {
            Self::AbortMismatch => "transport abort targets an applied revision",
            Self::DuplicateEvent => "session transport received duplicate events in one block",
            Self::FrameDiscontinuity => "graph render clock is discontinuous",
            Self::InvalidBeatRange => "session transport produced an invalid beat range",
            Self::MissingObservation => "transport observation store slot is missing",
            Self::MissingState => "transport commit state store slot is missing",
            Self::UnexpectedEvent => "session transport received an unexpected event",
        }
    }
}

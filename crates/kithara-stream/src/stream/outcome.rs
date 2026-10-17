use std::{io::Error as IoError, num::NonZeroUsize};

use crate::{PendingReason, SourcePhase};

/// Real error from [`crate::Stream::try_read`] — the underlying source
/// surfaced an I/O failure.
///
/// Status conditions (seek pending, data not ready, variant change,
/// retry) are **not** errors and are carried in
/// [`StreamReadOutcome::Pending`] with a typed [`PendingReason`]. Only
/// genuine source failures end up here.
#[derive(Debug, derive_more::Display, derive_more::Error)]
#[error(ignore)]
#[non_exhaustive]
pub enum StreamReadError {
    /// Anything surfaced by the underlying [`crate::Source`] as a real error.
    #[display("source error: {_0}")]
    Source(#[error(source)] IoError),
}

/// Outcome of a [`crate::Stream::try_read`] call.
///
/// Mirrors the [`crate::ReadOutcome`] shape from
/// [`Source::read_at`](crate::Source::read_at), but extends each variant
/// with the authoritative `byte_position` from the source cursor for
/// callers that don't want to read it back themselves.
/// `Bytes` carries a [`NonZeroUsize`] count — the type system
/// guarantees forward progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamReadOutcome {
    /// Stream produced `count` bytes. `byte_position` is the new byte
    /// offset **after** the read.
    Bytes {
        count: NonZeroUsize,
        byte_position: u64,
    },
    /// No progress this call. See [`PendingReason`] for the precise
    /// cause and required caller action.
    Pending(PendingReason),
    /// Natural end of stream. `byte_position` is the offset where EOF
    /// was observed (typically the source length).
    Eof { byte_position: u64 },
}

/// Typed [`std::io::ErrorKind::Interrupted`] payload carrying [`PendingReason`] and a source-state snapshot.
/// `NotReady`/`Retry` use `Interrupted` so demuxers treat partial reads as transient backpressure,
/// not terminal failures; callers downcast the payload instead of matching error messages.
#[derive(Debug, Clone, Copy, derive_more::Display)]
#[display("{reason}: pos={pos} want={want} len={len:?} phase={phase:?}")]
#[non_exhaustive]
#[derive(derive_more::Error)]
#[error(ignore)]
pub struct StreamPending {
    pub(crate) len: Option<u64>,
    pub(crate) reason: PendingReason,
    pub(crate) phase: SourcePhase,
    pub(crate) pos: u64,
    pub(crate) want: usize,
}

impl StreamPending {
    /// Build the typed payload for a transient "data not ready" read.
    #[must_use]
    pub const fn new(
        reason: PendingReason,
        pos: u64,
        want: usize,
        len: Option<u64>,
        phase: SourcePhase,
    ) -> Self {
        Self {
            len,
            reason,
            phase,
            pos,
            want,
        }
    }

    /// Typed reason decoders downcast on to classify a transient stall.
    #[must_use]
    pub const fn reason(&self) -> PendingReason {
        self.reason
    }
}

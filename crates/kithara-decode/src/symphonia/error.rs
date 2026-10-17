use std::io::ErrorKind;

use kithara_stream::{NotReadyCause, PendingReason, StreamPending, StreamSeekPastEof};
use symphonia_core::errors::{Error as SymphoniaError, SeekErrorKind};

use crate::DecodeError;

/// The typed payload behind `Interrupted` is lost by the time it reaches here, so only its variant
/// name can be logged.
pub(super) fn classify_seek_err(err: &SymphoniaError) -> DecodeError {
    match err {
        SymphoniaError::SeekError(SeekErrorKind::OutOfRange) => DecodeError::SeekOutOfRange {
            detail: "seek target past indexed sample range",
        },
        SymphoniaError::IoError(io_err)
            if io_err.get_ref().is_some_and(
                <dyn std::error::Error + Send + Sync + 'static>::is::<StreamSeekPastEof>,
            ) =>
        {
            DecodeError::SeekOutOfRange {
                detail: "seek past end of stream",
            }
        }
        SymphoniaError::IoError(e) if e.kind() == ErrorKind::UnexpectedEof => {
            DecodeError::SeekOutOfRange {
                detail: "seek hit unexpected end of stream",
            }
        }
        SymphoniaError::IoError(io_err)
            if matches!(
                io_err.kind(),
                ErrorKind::Interrupted | ErrorKind::WouldBlock
            ) =>
        {
            tracing::debug!(error = ?io_err, "demuxer seek interrupted");
            DecodeError::Interrupted
        }
        SymphoniaError::DecodeError(detail)
        | SymphoniaError::Unsupported(detail)
        | SymphoniaError::LimitError(detail) => DecodeError::SeekFailed { detail },
        SymphoniaError::SeekError(SeekErrorKind::ForwardOnly) => DecodeError::SeekFailed {
            detail: "a forward-only source refused a backward seek",
        },
        SymphoniaError::SeekError(SeekErrorKind::Unseekable) => DecodeError::SeekFailed {
            detail: "the source cannot seek",
        },
        SymphoniaError::SeekError(SeekErrorKind::InvalidTrack) => DecodeError::SeekFailed {
            detail: "the seek named a track the source does not carry",
        },
        SymphoniaError::ResetRequired => DecodeError::SeekFailed {
            detail: "the seek left the reader needing a reset",
        },
        SymphoniaError::IoError(_) => DecodeError::SeekFailed {
            detail: "the seek failed on an i/o error",
        },
        _ => DecodeError::SeekFailed {
            detail: "symphonia seek failed",
        },
    }
}

pub(super) fn pending_reason(error: &SymphoniaError) -> Option<PendingReason> {
    let SymphoniaError::IoError(error) = error else {
        return None;
    };
    if !matches!(error.kind(), ErrorKind::Interrupted | ErrorKind::WouldBlock) {
        return None;
    }
    Some(
        error
            .get_ref()
            .and_then(|source| {
                source
                    .downcast_ref::<StreamPending>()
                    .map(StreamPending::reason)
                    .or_else(|| source.downcast_ref::<PendingReason>().copied())
            })
            .unwrap_or(PendingReason::NotReady(NotReadyCause::SourcePending)),
    )
}

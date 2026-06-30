use std::{borrow::Cow, error::Error as StdError, io, io::ErrorKind, num::TryFromIntError};

use kithara_bufpool::BudgetExhausted;
use kithara_stream::{AudioCodec, ContainerFormat, PendingReason, VariantChangeError};
#[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
use kithara_stream::{NotReadyCause, StreamPending};
use thiserror::Error;

/// Errors that can occur during audio decoding.
///
/// This error type is backend-agnostic, wrapping decoder-specific errors
/// in the `Backend` variant.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum DecodeError {
    #[error("IO error: {0}")]
    Io(io::Error),

    #[error("Unsupported codec: {0:?}")]
    UnsupportedCodec(AudioCodec),

    #[error("Unsupported container: {0:?}")]
    UnsupportedContainer(ContainerFormat),

    #[error("Invalid data: {0}")]
    InvalidData(Cow<'static, str>),

    #[error("Seek failed: {0}")]
    SeekFailed(Cow<'static, str>),

    /// The seek target is invalid for this stream — past EOF, beyond the
    /// indexed sample range, or otherwise out of the addressable space.
    ///
    /// Distinct from [`SeekFailed`](Self::SeekFailed) because there is no
    /// recovery action that helps: the duration/length come from the
    /// stream itself, not from decoder state, so a freshly built decoder
    /// rejects the same target with the same answer. Pipeline must
    /// surface this to the caller (fail the seek) rather than retry.
    #[error("Seek target out of range: {0}")]
    SeekOutOfRange(Cow<'static, str>),

    #[error("Probe failed: could not detect codec")]
    ProbeFailed,

    /// The caller selected a backend that is not compiled in for the
    /// current target/feature combination (e.g. `DecoderBackend::Apple`
    /// on Linux, or `DecoderBackend::Symphonia` without the `symphonia`
    /// feature). Distinct from [`UnsupportedCodec`](Self::UnsupportedCodec)
    /// (codec/container the backend cannot handle): `BackendUnavailable`
    /// means the backend itself is absent from the binary.
    #[error("Backend unavailable: {backend}")]
    BackendUnavailable { backend: &'static str },

    #[error("invalid sample rate: zero is not a valid audio rate ({resource})")]
    InvalidSampleRate { resource: &'static str },

    /// A seek interrupted the decode operation. Not a real error —
    /// the caller should check for pending seeks and retry.
    #[error("Interrupted by seek")]
    Interrupted,

    #[error("Decoder error: {0}")]
    Backend(#[source] Box<dyn StdError + Send + Sync>),
}

fn is_seek_pending_io(err: &io::Error) -> bool {
    err.kind() == ErrorKind::Interrupted
        || err
            .get_ref()
            .and_then(|src| src.downcast_ref::<PendingReason>())
            .is_some_and(|reason| matches!(reason, PendingReason::SeekPending))
}

fn is_variant_change_io(err: &io::Error) -> bool {
    err.get_ref()
        .and_then(|source| source.downcast_ref::<VariantChangeError>())
        .is_some()
}

fn walk_error_chain<I, L>(err: &(dyn StdError + 'static), check_io: &I, check_leaf: &L) -> bool
where
    I: Fn(&io::Error) -> bool,
    L: Fn(&(dyn StdError + 'static)) -> bool,
{
    let io_hit = err.downcast_ref::<io::Error>().map(check_io);
    #[cfg(feature = "symphonia")]
    let symphonia_hit = crate::symphonia::echain::inspect(err, check_io, check_leaf);
    #[cfg(not(feature = "symphonia"))]
    let symphonia_hit: Option<bool> = None;
    let leaf_hit = check_leaf(err);
    match (io_hit, symphonia_hit, leaf_hit) {
        (Some(hit), _, _) | (None, Some(hit), _) => hit,
        (None, None, true) => true,
        (None, None, false) => err
            .source()
            .is_some_and(|source| walk_error_chain(source, check_io, check_leaf)),
    }
}

fn error_chain_is_interrupted(err: &(dyn StdError + 'static)) -> bool {
    walk_error_chain(err, &is_seek_pending_io, &|leaf| {
        leaf.downcast_ref::<PendingReason>()
            .is_some_and(|reason| matches!(reason, PendingReason::SeekPending))
    })
}

fn error_chain_is_variant_change(err: &(dyn StdError + 'static)) -> bool {
    walk_error_chain(err, &is_variant_change_io, &|leaf| {
        leaf.downcast_ref::<VariantChangeError>().is_some()
    })
}

/// Single-discriminant classification of a [`DecodeError`].
///
/// Walking the source chain via separate per-class boolean predicates
/// in sequence forces the chain to be traversed twice on the failure
/// path. [`DecodeError::classify`] runs the walk once and returns this
/// tag so callers can `match` instead of cascading boolean predicates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ErrorClass {
    /// Interrupted by seek / cooperative pending — caller should retry.
    Interrupted,
    /// Cross-variant boundary — caller must recreate the decoder.
    VariantChange,
    /// Anything else — caller surfaces as terminal failure.
    Other,
}

impl DecodeError {
    /// Wrap any `StdError + Send + Sync` payload as a [`DecodeError::Backend`].
    /// Avoids 30+ repeats of `.map_err(|e| DecodeError::Backend(Box::new(e)))`
    /// across the apple / android / symphonia codec layers.
    #[must_use]
    pub fn backend<E>(err: E) -> Self
    where
        E: Into<Box<dyn StdError + Send + Sync>>,
    {
        Self::Backend(err.into())
    }

    /// Convenience constructor for "we have a description string, not
    /// a typed source error" — typically `format!(...)` payloads for
    /// FFI status codes (Apple `OSStatus`, Android `media_status_t`).
    #[must_use]
    pub fn backend_msg<S>(msg: S) -> Self
    where
        S: Into<String>,
    {
        Self::Backend(Box::new(io::Error::other(msg.into())))
    }

    /// Tag the error in one source-chain pass so hot decode loops can
    /// replace per-class boolean predicate ladders with a single
    /// `match` over the discriminant.
    #[must_use]
    #[inline]
    // ast-grep-ignore: idioms.match-self-conversion
    pub fn classify(&self) -> ErrorClass {
        match self {
            Self::Interrupted => ErrorClass::Interrupted,
            Self::Io(err) => {
                if is_variant_change_io(err) {
                    ErrorClass::VariantChange
                } else if is_seek_pending_io(err) {
                    ErrorClass::Interrupted
                } else {
                    ErrorClass::Other
                }
            }
            Self::Backend(err) => {
                let leaf = err.as_ref();
                if error_chain_is_variant_change(leaf) {
                    ErrorClass::VariantChange
                } else if error_chain_is_interrupted(leaf) {
                    ErrorClass::Interrupted
                } else {
                    ErrorClass::Other
                }
            }
            _ => ErrorClass::Other,
        }
    }

    /// Returns `true` if the error is an [`Interrupted`](Self::Interrupted) variant.
    #[must_use]
    pub fn is_interrupted(&self) -> bool {
        matches!(self.classify(), ErrorClass::Interrupted)
    }

    /// Typed [`PendingReason`] carried by a transient source-read error
    /// (`Interrupted`/`WouldBlock` `io::Error` with a `StreamPending` /
    /// `PendingReason` payload). Demuxers use this to honour the
    /// [`Demuxer::next_frame`](crate::demuxer::Demuxer::next_frame)
    /// contract — "data not ready" returns `Ok(DemuxOutcome::Pending)`,
    /// never `Err`. Mirrors the Symphonia demuxer's not-ready guard
    /// (including the bare-`Interrupted` → `SourcePending` default).
    #[must_use]
    #[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
    pub(crate) fn pending_reason(&self) -> Option<PendingReason> {
        let io_err = match self {
            Self::Io(e) => e,
            Self::Backend(b) => b.as_ref().downcast_ref::<io::Error>()?,
            _ => return None,
        };
        if io_err.kind() != ErrorKind::Interrupted && io_err.kind() != ErrorKind::WouldBlock {
            return None;
        }
        let reason = io_err
            .get_ref()
            .and_then(|src| {
                src.downcast_ref::<StreamPending>()
                    .map(|p| p.reason)
                    .or_else(|| src.downcast_ref::<PendingReason>().copied())
            })
            .unwrap_or(PendingReason::NotReady(NotReadyCause::SourcePending));
        Some(reason)
    }
}

impl From<io::Error> for DecodeError {
    fn from(err: io::Error) -> Self {
        if err.kind() == ErrorKind::Interrupted {
            Self::Interrupted
        } else {
            Self::Io(err)
        }
    }
}

impl From<TryFromIntError> for DecodeError {
    fn from(err: TryFromIntError) -> Self {
        Self::backend(err)
    }
}

impl From<BudgetExhausted> for DecodeError {
    fn from(err: BudgetExhausted) -> Self {
        Self::backend(err)
    }
}

/// Result type for decode operations.
pub type DecodeResult<T> = Result<T, DecodeError>;

#[cfg(test)]
mod tests {
    use std::io::{Error as IoError, ErrorKind};

    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    #[case::invalid_data(DecodeError::InvalidData("bad frame".into()), "Invalid data: bad frame")]
    #[case::seek_failed(DecodeError::SeekFailed("timestamp out of range".into()), "Seek failed: timestamp out of range")]
    #[case::probe_failed(DecodeError::ProbeFailed, "Probe failed: could not detect codec")]
    #[case::unsupported_codec(
        DecodeError::UnsupportedCodec(AudioCodec::AacLc),
        "Unsupported codec: AacLc"
    )]
    #[case::unsupported_container(
        DecodeError::UnsupportedContainer(ContainerFormat::Fmp4),
        "Unsupported container: Fmp4"
    )]
    fn test_error_display(#[case] error: DecodeError, #[case] expected: &str) {
        assert_eq!(error.to_string(), expected);
    }

    #[derive(Debug, Clone, Copy)]
    enum ExpectedKind {
        Io,
        Interrupted,
    }

    #[kithara::test]
    #[case::not_found_becomes_io(ErrorKind::NotFound, "file not found", ExpectedKind::Io)]
    #[case::interrupted_becomes_interrupted(
        ErrorKind::Interrupted,
        "seek pending",
        ExpectedKind::Interrupted
    )]
    fn test_decode_error_from_io(
        #[case] kind: ErrorKind,
        #[case] msg: &str,
        #[case] expected: ExpectedKind,
    ) {
        let io_err = IoError::new(kind, msg);
        let decode_err: DecodeError = io_err.into();
        match expected {
            ExpectedKind::Io => assert!(matches!(decode_err, DecodeError::Io(_))),
            ExpectedKind::Interrupted => assert!(matches!(decode_err, DecodeError::Interrupted)),
        }
    }

    #[kithara::test]
    fn test_decode_error_backend_wraps_any_error() {
        let err = DecodeError::backend(IoError::other("symphonia error"));
        assert!(err.to_string().contains("Decoder error"));
    }

    #[kithara::test]
    fn test_decode_error_backend_msg_wraps_a_display() {
        let err = DecodeError::backend_msg(format!("oss status {}", 42));
        assert!(err.to_string().contains("oss status 42"));
    }

    #[kithara::test]
    fn test_decode_error_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<DecodeError>();
    }

    #[kithara::test]
    #[case::seek_pending_counts_as_interrupted(
        DecodeError::backend(IoError::other(PendingReason::SeekPending)),
        true
    )]
    #[case::other_io_is_not_interrupted(
        DecodeError::backend(IoError::other("other backend error")),
        false
    )]
    fn test_backend_is_interrupted(#[case] decode_err: DecodeError, #[case] expected: bool) {
        assert_eq!(decode_err.is_interrupted(), expected);
    }

    #[kithara::test]
    fn test_io_variant_change_is_detected() {
        let decode_err = DecodeError::Io(IoError::other(VariantChangeError));
        assert_eq!(decode_err.classify(), ErrorClass::VariantChange);
        assert!(!decode_err.is_interrupted());
    }
}

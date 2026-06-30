use std::{error::Error as StdError, io, io::ErrorKind, num::TryFromIntError};

use kithara_bufpool::BudgetExhausted;
use kithara_stream::{AudioCodec, ContainerFormat, PendingReason, VariantChangeError};
#[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
use kithara_stream::{NotReadyCause, StreamPending};

/// Errors that can occur during audio decoding.
///
/// This error type is backend-agnostic, wrapping decoder-specific errors
/// in the `Backend` variant.
///
/// The real-time decode core constructs [`InvalidData`](Self::InvalidData),
/// [`SeekFailed`](Self::SeekFailed) and [`SeekOutOfRange`](Self::SeekOutOfRange)
/// on the `#[kithara::rtsan_forbid_blocking]` produce path, so those variants
/// carry only `&'static str` / `Copy` fields — construction never allocates.
/// `Display` formatting may allocate, but only off-RT when actually rendered.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DecodeError {
    #[error("IO error: {source}")]
    Io {
        #[source]
        source: io::Error,
    },

    #[error("Unsupported codec: {codec:?}")]
    UnsupportedCodec { codec: AudioCodec },

    #[error("Unsupported container: {container:?}")]
    UnsupportedContainer { container: ContainerFormat },

    #[error("Invalid data: {detail}")]
    InvalidData { detail: &'static str },

    #[error("Seek failed: {detail}")]
    SeekFailed { detail: &'static str },

    /// The seek target is invalid for this stream — past EOF, beyond the
    /// indexed sample range, or otherwise out of the addressable space.
    ///
    /// Distinct from [`SeekFailed`](Self::SeekFailed) because there is no
    /// recovery action that helps: the duration/length come from the
    /// stream itself, not from decoder state, so a freshly built decoder
    /// rejects the same target with the same answer. Pipeline must
    /// surface this to the caller (fail the seek) rather than retry.
    #[error("Seek target out of range: {detail}")]
    SeekOutOfRange { detail: &'static str },

    /// Off-RT parse failure carrying the upstream parser/io error as a
    /// typed source. Used by the init-segment / box parsers, which run on
    /// in-memory buffers during decoder construction (never on the RT
    /// produce path).
    #[error("Parse failed in {what}: {source}")]
    Parse {
        what: &'static str,
        #[source]
        source: Box<dyn StdError + Send + Sync>,
    },

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

    /// A platform decode backend reported a numeric status code (Apple
    /// `OSStatus`, Android `media_status_t`). Carries the raw code plus a
    /// `&'static str` operation tag instead of a pre-formatted string.
    #[error("decoder backend status {code} ({op})")]
    BackendStatus { code: i32, op: &'static str },

    /// A seek interrupted the decode operation. Not a real error —
    /// the caller should check for pending seeks and retry.
    #[error("Interrupted by seek")]
    Interrupted,

    #[error("Decoder error: {source}")]
    Backend {
        #[source]
        source: Box<dyn StdError + Send + Sync>,
    },
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
    /// Avoids 30+ repeats of `.map_err(|e| DecodeError::Backend { source: Box::new(e) })`
    /// across the apple / android / symphonia codec layers.
    #[must_use]
    pub fn backend<E>(err: E) -> Self
    where
        E: Into<Box<dyn StdError + Send + Sync>>,
    {
        Self::Backend { source: err.into() }
    }

    /// Wrap an upstream parser / io error as a [`DecodeError::Parse`] with a
    /// `&'static str` site tag. For off-RT init-segment / box parsing only —
    /// preserves the typed source chain without stringifying it.
    #[must_use]
    pub fn parse<E>(what: &'static str, err: E) -> Self
    where
        E: Into<Box<dyn StdError + Send + Sync>>,
    {
        Self::Parse {
            what,
            source: err.into(),
        }
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
            Self::Io { source } => {
                if is_variant_change_io(source) {
                    ErrorClass::VariantChange
                } else if is_seek_pending_io(source) {
                    ErrorClass::Interrupted
                } else {
                    ErrorClass::Other
                }
            }
            Self::Backend { source } => {
                let leaf = source.as_ref();
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
            Self::Io { source } => source,
            Self::Backend { source } => source.as_ref().downcast_ref::<io::Error>()?,
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
            Self::Io { source: err }
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
    #[case::invalid_data(DecodeError::InvalidData { detail: "bad frame" }, "Invalid data: bad frame")]
    #[case::seek_failed(DecodeError::SeekFailed { detail: "timestamp out of range" }, "Seek failed: timestamp out of range")]
    #[case::probe_failed(DecodeError::ProbeFailed, "Probe failed: could not detect codec")]
    #[case::unsupported_codec(
        DecodeError::UnsupportedCodec { codec: AudioCodec::AacLc },
        "Unsupported codec: AacLc"
    )]
    #[case::unsupported_container(
        DecodeError::UnsupportedContainer { container: ContainerFormat::Fmp4 },
        "Unsupported container: Fmp4"
    )]
    #[case::backend_status(
        DecodeError::BackendStatus { code: -42, op: "AudioFileOpen" },
        "decoder backend status -42 (AudioFileOpen)"
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
            ExpectedKind::Io => assert!(matches!(decode_err, DecodeError::Io { .. })),
            ExpectedKind::Interrupted => assert!(matches!(decode_err, DecodeError::Interrupted)),
        }
    }

    #[kithara::test]
    fn test_decode_error_backend_wraps_any_error() {
        let err = DecodeError::backend(IoError::other("symphonia error"));
        assert!(err.to_string().contains("Decoder error"));
    }

    #[kithara::test]
    fn test_decode_error_parse_preserves_source() {
        let err = DecodeError::parse("re_mp4", IoError::other("truncated box"));
        assert!(err.to_string().contains("Parse failed in re_mp4"));
        assert!(StdError::source(&err).is_some());
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
        let decode_err = DecodeError::Io {
            source: IoError::other(VariantChangeError),
        };
        assert_eq!(decode_err.classify(), ErrorClass::VariantChange);
        assert!(!decode_err.is_interrupted());
    }
}

#![forbid(unsafe_code)]

use thiserror::Error;

/// Errors produced by `kithara-stream` (generic over source error).
///
/// Notes:
/// - `Source(E)` is for domain-specific failures (network/storage/etc) surfaced by a concrete
///   `Source` implementation.
/// - `ChannelClosed` is returned when the consumer dropped the output stream or the command
///   channel is no longer available.
/// - `SeekNotSupported` is for sources that don't implement seek.
/// - `WriterJoin` is for unexpected writer task join failures (panic/cancellation).
#[derive(Debug, Error)]
pub enum StreamError<E>
where
    E: std::error::Error + Send + Sync + 'static,
{
    #[error("seek not supported")]
    SeekNotSupported,

    #[error("invalid seek position")]
    InvalidSeek,

    #[error("seek requires known length, but source length is unknown")]
    UnknownLength,

    #[error("source error: {0}")]
    Source(#[source] E),

    #[error("internal channel closed")]
    ChannelClosed,

    #[error("writer task join error: {0}")]
    WriterJoin(String),
}

/// Result type for `kithara-stream` (generic over source error).
pub type StreamResult<T, E> = Result<T, StreamError<E>>;

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::seek_not_supported(StreamError::<std::io::Error>::SeekNotSupported, "seek not supported")]
    #[case::invalid_seek(StreamError::<std::io::Error>::InvalidSeek, "invalid seek position")]
    #[case::unknown_length(StreamError::<std::io::Error>::UnknownLength, "seek requires known length, but source length is unknown")]
    #[case::channel_closed(StreamError::<std::io::Error>::ChannelClosed, "internal channel closed")]
    #[case::writer_join(StreamError::<std::io::Error>::WriterJoin("panic message".into()), "writer task join error: panic message")]
    #[test]
    fn test_error_display(#[case] error: StreamError<std::io::Error>, #[case] expected: &str) {
        assert_eq!(error.to_string(), expected);
    }

    #[test]
    fn test_source_error_display() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file missing");
        let err: StreamError<std::io::Error> = StreamError::Source(io_err);
        assert_eq!(err.to_string(), "source error: file missing");
    }

    #[test]
    fn test_stream_error_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<StreamError<std::io::Error>>();
    }
}

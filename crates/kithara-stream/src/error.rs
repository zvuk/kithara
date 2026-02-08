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
    use super::*;

    #[test]
    fn test_seek_not_supported_display() {
        let err: StreamError<std::io::Error> = StreamError::SeekNotSupported;
        assert_eq!(err.to_string(), "seek not supported");
    }

    #[test]
    fn test_invalid_seek_display() {
        let err: StreamError<std::io::Error> = StreamError::InvalidSeek;
        assert_eq!(err.to_string(), "invalid seek position");
    }

    #[test]
    fn test_unknown_length_display() {
        let err: StreamError<std::io::Error> = StreamError::UnknownLength;
        assert_eq!(
            err.to_string(),
            "seek requires known length, but source length is unknown"
        );
    }

    #[test]
    fn test_channel_closed_display() {
        let err: StreamError<std::io::Error> = StreamError::ChannelClosed;
        assert_eq!(err.to_string(), "internal channel closed");
    }

    #[test]
    fn test_writer_join_display() {
        let err: StreamError<std::io::Error> = StreamError::WriterJoin("panic message".into());
        assert_eq!(err.to_string(), "writer task join error: panic message");
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

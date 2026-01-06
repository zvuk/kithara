#![forbid(unsafe_code)]

use thiserror::Error;

/// Errors produced by `kithara-stream` (generic over source error).
///
/// Notes:
/// - `Source(E)` is for domain-specific failures (network/storage/etc) surfaced by a concrete
///   `EngineSource` implementation.
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

    #[error("source error: {0}")]
    Source(#[source] E),

    #[error("internal channel closed")]
    ChannelClosed,

    #[error("writer task join error: {0}")]
    WriterJoin(String),
}

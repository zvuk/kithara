use kithara_encode::EncodeError;
use thiserror::Error;

/// Errors raised while packaging a live stream.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum BroadcastError {
    #[error("ADTS carries no sampling frequency index for {sample_rate} Hz")]
    UnsupportedSampleRate { sample_rate: u32 },

    #[error("ADTS carries no channel configuration for {channels} channels")]
    UnsupportedChannels { channels: u16 },

    #[error("access unit of {payload} bytes exceeds the {limit}-byte ADTS frame payload")]
    PayloadTooLarge { payload: usize, limit: usize },

    #[error("segment duration {duration_ts} does not fit the playlist time base")]
    DurationOutOfRange { duration_ts: u64 },

    #[error("{field} must be > 0")]
    InvalidConfig { field: &'static str },

    #[error("encode error: {0}")]
    Encode(#[from] EncodeError),
}

/// Result type for live packaging operations.
pub type BroadcastResult<T> = Result<T, BroadcastError>;

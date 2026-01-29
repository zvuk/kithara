#![forbid(unsafe_code)]

//! Decoder events for monitoring.

use std::time::Duration;

use crate::types::PcmSpec;

/// Events from the decoder.
#[derive(Debug, Clone)]
pub enum DecodeEvent {
    /// Audio format detected.
    FormatDetected { spec: PcmSpec },
    /// Audio format changed (ABR switch).
    FormatChanged { old: PcmSpec, new: PcmSpec },
    /// Seek completed.
    SeekComplete { position: Duration },
    /// Decoding finished (EOF).
    EndOfStream,
}

/// Unified decoder event stream.
///
/// `E` is the stream event type (`HlsEvent`, `FileEvent`).
#[derive(Debug, Clone)]
pub enum DecoderEvent<E> {
    /// Event from the underlying stream.
    Stream(E),
    /// Event from the decoder.
    Decode(DecodeEvent),
}

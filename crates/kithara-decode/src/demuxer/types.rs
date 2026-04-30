//! Demuxer outcome / metadata types.

use std::time::Duration;

use kithara_stream::{AudioCodec, PendingReason};

/// Track-level metadata produced by [`super::Demuxer::track_info`].
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct TrackInfo {
    /// Audio codec carried by this track.
    pub codec: AudioCodec,
    /// Decoded sample rate (Hz).
    pub sample_rate: u32,
    /// Channel count.
    pub channels: u16,
    /// Container timescale (PTS unit, e.g. `mdhd.timescale` in mp4).
    pub timescale: u32,
    /// Codec-specific extra data — `AudioSpecificConfig` (AAC),
    /// `STREAMINFO` (FLAC), `esds` cookie (Apple), etc. Empty when the
    /// codec needs no extra data.
    pub extra_data: Vec<u8>,
    /// Total track duration if available.
    pub duration: Option<Duration>,
}

/// One demuxed audio frame.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Frame {
    /// Raw frame bytes — owned so the caller can reuse / queue them
    /// without holding the demuxer's internal buffer.
    pub data: Vec<u8>,
    /// Presentation time of this frame.
    pub pts: Duration,
    /// Frame duration.
    pub duration: Duration,
}

/// Result of a [`super::Demuxer::next_frame`] call.
#[derive(Debug)]
pub enum DemuxOutcome {
    /// One frame demuxed. Caller routes it to the codec layer.
    Frame(Frame),
    /// No frame available right now — caller should re-poll later.
    Pending(PendingReason),
    /// Natural end of stream.
    Eof,
}

/// Result of a [`super::Demuxer::seek`] call.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub enum DemuxSeekOutcome {
    /// Successfully landed inside the stream. `landed_at` is the
    /// authoritative target (≤ requested target). `landed_byte` is the
    /// optional byte-level cursor for source-level reconciliation.
    Landed {
        landed_at: Duration,
        landed_byte: Option<u64>,
    },
    /// The seek target lies past the stream's end; `duration` is the
    /// total stream duration.
    PastEof { duration: Duration },
}

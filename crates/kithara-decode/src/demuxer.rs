use kithara_platform::time::Duration;
pub(crate) use kithara_stream::PrerollHint;
use kithara_stream::{AudioCodec, PendingReason};

use crate::{InputRequirement, codec::CodecPriming, error::DecodeResult};

/// Container-side demuxer trait.
///
/// Implementations parse a container (HLS-fmp4, file-mp4, MP3, OGG, …)
/// and emit raw codec frames with timing metadata. The codec layer
/// ([`crate::codec::FrameCodec`]) consumes those frames into PCM.
pub(crate) trait Demuxer: Send {
    /// Segment index of the frame from the last `next_frame`.
    /// `None` for non-segmented sources.
    fn current_segment_index(&self) -> Option<u32> {
        None
    }

    /// Variant index of the frame from the last `next_frame`.
    /// `None` for non-segmented sources.
    fn current_variant_index(&self) -> Option<usize> {
        None
    }

    /// Total duration if the container can compute one (HLS playlist
    /// total, mp4 `mvhd`, …); `None` for live or unbounded streams.
    fn duration(&self) -> Option<Duration>;

    /// Pull the next demuxed frame, borrowing the bytes from internal
    /// demuxer state. The caller must consume the frame (typically by
    /// passing it to a [`crate::codec::FrameCodec`]) before calling
    /// `next_frame` again — the `Frame<'_>` borrow scope ends with the
    /// next mutable call on `self`.
    ///
    /// # Errors
    ///
    /// Surfaces parser-level failures verbatim. Source-level pending
    /// states return `Ok(DemuxOutcome::Pending(_))`.
    fn next_frame(&mut self) -> DecodeResult<DemuxOutcome<'_>>;

    /// Seek the demuxer to `target` time.
    ///
    /// `priming` carries the codec's pre-roll requirements — packets/frames
    /// the demuxer should back off before `target` so the codec layer can
    /// decode-and-discard warm-up data. Demuxers that do not support
    /// byte-accurate pre-roll (Android, Apple `AudioFile`) may ignore the
    /// field and return `PrerollHint::NotNeeded`.
    ///
    /// Returns the actual landing point — `Landed { landed_at }` for a
    /// successful seek, `PastEof { duration }` when the target lies
    /// beyond the stream's known length.
    ///
    /// # Errors
    ///
    /// Surfaces parser-level seek failures verbatim.
    fn seek(&mut self, target: Duration, priming: CodecPriming) -> DecodeResult<DemuxSeekOutcome>;

    /// Track-level metadata exposed by the container.
    fn track_info(&self) -> &TrackInfo;

    /// The *shape* of bytes that must be Ready before this demuxer can be
    /// constructed, for the kithara-audio readiness gate. Defaults to
    /// [`InputRequirement::Incremental`]; init-bearing demuxers (fMP4) MUST
    /// override to [`InputRequirement::InitOnly`]. See the crate `README.md`
    /// "Decoder input contract".
    fn required_input() -> InputRequirement
    where
        Self: Sized,
    {
        InputRequirement::Incremental
    }
}

/// Track-level metadata produced by [`Demuxer::track_info`].
#[derive(Clone, Debug)]
#[non_exhaustive]
pub(crate) struct TrackInfo {
    /// Audio codec carried by this track.
    pub(crate) codec: AudioCodec,
    /// Total track duration if available.
    pub(crate) duration: Option<Duration>,
    /// Container-level gapless metadata — populated by demuxers that
    /// can extract it without consuming the decoder (MP4 `iTunSMPB`
    /// or track `elst`, FLAC `padded_sample_count`, etc.). `None` when
    /// the demuxer either skipped probing (gapless disabled) or saw no
    /// recognised source. Codec-level capture (`AppleCodec` `PrimeInfo`
    /// refresh, `Symphonia` `AudioDecoderOptions::gapless`) supplements
    /// this for codecs whose priming is not container-visible.
    pub(crate) gapless: Option<crate::GaplessInfo>,
    /// Codec-specific extra data — `AudioSpecificConfig` (AAC),
    /// `STREAMINFO` (FLAC), `esds` cookie (Apple), etc. Empty when the
    /// codec needs no extra data.
    pub(crate) extra_data: Vec<u8>,
    /// Channel count.
    pub(crate) channels: u16,
    /// Decoded sample rate (Hz).
    pub(crate) sample_rate: u32,
}

/// One demuxed audio frame, borrowed from the demuxer's internal state.
/// The borrow lifetime is tied to the `&mut self` of the producing
/// `next_frame` call — the codec layer consumes it on the same loop
/// iteration, so the lifetime never escapes [`Demuxer::next_frame`].
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub(crate) struct Frame<'a> {
    /// Raw frame bytes — slice into the demuxer's owned buffer (mp4
    /// segment, Symphonia `Packet`, etc.). Zero-copy: never cloned.
    pub(crate) data: &'a [u8],
    /// Opaque per-packet metadata for codecs that need it for VBR
    /// decoding. Apple-native MP3 / ALAC paths pass a serialized
    /// `AudioStreamPacketDescription` here; the codec interprets the
    /// bytes. Demuxers without VBR descriptors leave this empty.
    /// Borrow lifetime mirrors `data`.
    pub(crate) packet_desc: &'a [u8],
    /// Frame duration.
    pub(crate) duration: Duration,
    /// Presentation time of this frame.
    pub(crate) pts: Duration,
}

/// Result of a [`Demuxer::next_frame`] call.
#[derive(Debug)]
pub(crate) enum DemuxOutcome<'a> {
    /// One frame demuxed. Caller routes it to the codec layer.
    Frame(Frame<'a>),
    /// No frame available right now — caller should re-poll later.
    Pending(PendingReason),
    /// Natural end of stream.
    Eof,
}

/// Result of a [`Demuxer::seek`] call.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub(crate) enum DemuxSeekOutcome {
    /// Successfully landed inside the stream. `landed_at` is the
    /// authoritative target (≤ requested target). `landed_byte` is the
    /// optional byte-level cursor where playback continues.
    Landed {
        landed_at: Duration,
        landed_byte: Option<u64>,
        /// Codec priming hint. See `kithara-decode` README §Seek priming.
        preroll: PrerollHint,
    },
    /// The seek target lies past the stream's end; `duration` is the
    /// total stream duration.
    PastEof { duration: Duration },
}

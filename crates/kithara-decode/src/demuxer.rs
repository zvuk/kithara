use kithara_platform::time::Duration;
pub(crate) use kithara_stream::PrerollHint;
use kithara_stream::{AudioCodec, PendingReason};

use crate::{InputState, PrepareWait, codec::CodecPriming, error::DecodeResult};

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
    fn duration(&self) -> Option<crate::DecoderDuration>;

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

    /// Borrow a frame using only resources prepared by `prepare_frame`.
    ///
    /// # Errors
    /// Returns the same frame errors as `next_frame`.
    fn next_frame_prepared(&mut self) -> DecodeResult<DemuxOutcome<'_>> {
        self.next_frame()
    }

    /// Prepare container storage and metadata outside the real-time decode core.
    /// Segment and variant queries describe this prepared packet before it is borrowed.
    ///
    /// # Errors
    /// Returns source, parser, or pool-budget failures encountered during preparation.
    fn prepare_frame(&mut self) -> DecodeResult<()> {
        Ok(())
    }

    fn prepare_frame_with_wait(&mut self, _wait: PrepareWait) -> DecodeResult<()> {
        self.prepare_frame()
    }

    fn release_input(&mut self) -> InputState {
        InputState::Idle
    }

    /// Seek to `target`, backing off by `priming` for codec warm-up when supported.
    /// Android and Apple `AudioFile` may ignore pre-roll and return `PrerollHint::NotNeeded`.
    /// Returns the actual landing point or `PastEof` beyond the known extent.
    ///
    /// # Errors
    /// Surfaces parser-level seek failures verbatim.
    fn seek(&mut self, target: Duration, priming: CodecPriming) -> DecodeResult<DemuxSeekOutcome>;

    fn seek_with_wait(
        &mut self,
        target: Duration,
        priming: CodecPriming,
        _wait: PrepareWait,
    ) -> DecodeResult<DemuxSeekOutcome> {
        self.seek(target, priming)
    }

    /// Continue an unresolved seek, returning its settlement once.
    ///
    /// # Errors
    /// Returns source or parser failures from the same seek operation.
    fn prepare_seek(&mut self) -> DecodeResult<Option<DemuxSeekOutcome>> {
        Ok(None)
    }

    fn prepare_seek_with_wait(
        &mut self,
        _wait: PrepareWait,
    ) -> DecodeResult<Option<DemuxSeekOutcome>> {
        self.prepare_seek()
    }

    /// Raw packet coordinate of native media zero; absent when the translation is unknown.
    fn media_origin(&self) -> Option<Duration> {
        None
    }

    /// Track-level metadata exposed by the container.
    fn track_info(&self) -> &TrackInfo;
}

/// Track-level metadata produced by [`Demuxer::track_info`].
#[derive(Clone, Debug)]
#[non_exhaustive]
pub(crate) struct TrackInfo {
    /// Audio codec carried by this track.
    pub(crate) codec: AudioCodec,
    /// Total track duration if available.
    pub(crate) duration: Option<crate::DecoderDuration>,
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

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct PacketMetadata {
    pub(crate) segment_index: Option<u32>,
    pub(crate) variant_index: Option<usize>,
    pub(crate) source_bytes: u64,
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
    /// Accepted seek with no landing coordinate yet; continue through `prepare_seek`.
    Pending(PendingReason),
    /// Successfully landed inside the stream. `landed_at` is the
    /// authoritative target (≤ requested target). `landed_byte` is the
    /// optional byte-level cursor where playback continues.
    Landed {
        landed_at: Duration,
        landed_byte: Option<u64>,
        /// Codec priming hint.
        preroll: PrerollHint,
    },
    /// The seek target lies past the stream's end; `duration` is the
    /// total stream duration.
    PastEof { duration: Duration },
}

/// Timing and pending state for a packet retained in its demuxer's buffer.
#[cfg(symphonia_demuxer)]
pub(crate) enum PreparedPacket {
    Frame { pts: Duration, duration: Duration },
    Pending(PendingReason),
    Eof,
}

#[cfg(symphonia_demuxer)]
impl From<DemuxOutcome<'_>> for PreparedPacket {
    fn from(outcome: DemuxOutcome<'_>) -> Self {
        match outcome {
            DemuxOutcome::Frame(frame) => Self::Frame {
                pts: frame.pts,
                duration: frame.duration,
            },
            DemuxOutcome::Pending(reason) => Self::Pending(reason),
            DemuxOutcome::Eof => Self::Eof,
        }
    }
}

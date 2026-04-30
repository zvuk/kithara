use crate::{
    error::DecodeResult,
    fmp4_segment::demux::{Fmp4Frame, Fmp4InitInfo},
    types::PcmSpec,
};

/// Output of one frame decode.
///
/// Interleaved PCM samples; frame count = `samples.len() / channels`.
#[derive(Debug)]
pub(crate) struct DecodedFrame {
    pub(crate) samples: Vec<f32>,
    pub(crate) frames: u32,
}

/// Frame-level codec contract used by [`super::decoder::Fmp4SegmentDecoder`].
///
/// Implementations consume one demuxed AAC / FLAC frame at a time
/// (already extracted from `mdat` by [`super::demux::parse_segment_frames`])
/// and produce interleaved f32 PCM. They never see container bytes —
/// container parsing is the demuxer's job.
pub(crate) trait FrameCodec: Send + 'static {
    /// Construct a codec from the parsed init segment.
    fn open(init: &Fmp4InitInfo) -> DecodeResult<Self>
    where
        Self: Sized;

    /// Decode one demuxed frame.
    ///
    /// `frame_data` is the raw bytes for this frame extracted from the
    /// segment buffer by the demuxer. `frame` carries the timing
    /// metadata; codecs may ignore it (decoded samples already imply
    /// duration via frame count + `sample_rate`).
    fn decode_frame(&mut self, frame: &Fmp4Frame, frame_data: &[u8]) -> DecodeResult<DecodedFrame>;

    /// Reset internal codec state — called after seek.
    fn flush(&mut self);

    /// PCM output specification.
    fn spec(&self) -> PcmSpec;
}

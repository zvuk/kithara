use kithara_bufpool::PcmBuf;
use kithara_platform::time::Duration;
use kithara_stream::AudioCodec;

use crate::{error::DecodeResult, types::PcmSpec};

#[derive(Debug, Default, Clone, Copy, Eq, PartialEq)]
#[non_exhaustive]
pub struct CodecPriming {
    pub packets: u32,
    pub byte_margin: u64,
    pub frames: u64,
}

/// PCM frames per coded access unit (AAC 1024, MP3 1152). `0` for codecs
/// without a fixed AU size. Converts a seek warm-up packet count into a
/// back-off duration (`packets * access_unit_frames / sample_rate`).
pub(crate) fn access_unit_frames(codec: AudioCodec) -> u32 {
    match codec {
        AudioCodec::Mp3 => 1152,
        AudioCodec::AacLc | AudioCodec::AacHe | AudioCodec::AacHeV2 => 1024,
        _ => 0,
    }
}

/// Frame-level codec contract paired with a [`crate::demuxer::Demuxer`] in
/// `ComposedDecoder<D, C>`.
///
/// Implementations consume one demuxed frame at a time and write
/// interleaved f32 PCM into the caller-provided pool buffer (`out`).
/// They never see container bytes — container parsing is the demuxer's
/// job. They never allocate their own output `Vec<f32>` — output flows
/// through the injected `PcmPool`, which keeps the hot path zero-alloc
/// once the pool is warm.
pub(crate) trait FrameCodec: Send + 'static {
    /// Decode one demuxed frame, writing interleaved f32 samples into
    /// `out` (which the caller acquired from the shared `PcmPool`).
    /// Returns the frame count actually written (each frame is
    /// `spec.channels` samples). `0` means the codec consumed the input
    /// but produced no PCM (warm-up packet, codec backpressure, etc.) —
    /// caller should pull the next frame.
    ///
    /// `frame_data` is the raw bytes for this frame. `pts` is the
    /// presentation time supplied by the demuxer; codecs may use it
    /// for diagnostics — decoded sample count + sample rate are the
    /// authoritative duration source.
    ///
    /// `packet_desc` carries opaque per-packet metadata from the
    /// demuxer for VBR codecs (Apple MP3/ALAC pass a serialized
    /// `AudioStreamPacketDescription` here). Empty when the demuxer
    /// produces CBR frames or doesn't model per-packet descriptors;
    /// codecs that don't need it ignore the slot.
    ///
    /// # Errors
    ///
    /// Returns a [`crate::DecodeError`] when the underlying decoder
    /// produces an unrecoverable error or when growing `out` would
    /// exceed the pool's byte budget.
    fn decode_frame(
        &mut self,
        frame_data: &[u8],
        pts: Duration,
        packet_desc: &[u8],
        out: &mut PcmBuf,
    ) -> DecodeResult<u32>;

    /// Decoder-side algorithmic delay in PCM frames for `codec` — the
    /// silent lead-in this concrete decoder emits **in addition to**
    /// the encoder-declared priming. LAME-convention MP3 decoders
    /// (Symphonia `mpa` and Apple `AudioConverter`) report 529 here;
    /// other codecs default to 0.
    ///
    /// `kithara_audio::pipeline::gapless::resolve_codec_priming`
    /// combines this with [`AudioCodec::encoder_priming_frames`] for
    /// the [`crate::GaplessMode::CodecPriming`] fallback when probing
    /// yields no metadata; codec `open_with_config` impls fold it into
    /// the probed `GaplessInfo` so callers downstream see a single
    /// fully-resolved trim.
    fn decoder_algo_delay(&self, _codec: AudioCodec) -> u64 {
        0
    }

    /// Reset internal codec state — called after seek. Backends that
    /// can fail to reset (Android `AMediaCodec_flush`) propagate the
    /// error through `DecodeResult` so the seek path can surface it.
    ///
    /// # Errors
    ///
    /// Returns [`crate::DecodeError`] when the codec rejects the
    /// reset.
    fn flush(&mut self) -> DecodeResult<()>;

    /// Seek priming requirements for `codec` — packets/frames/bytes the
    /// demuxer must back off before the seek target so this codec can
    /// fully prime its MDCT/SBR overlap-add state. Default returns
    /// [`CodecPriming::default()`] (no priming required). Backends that
    /// manage priming internally (e.g. Symphonia fdk-aac / mpa) keep the
    /// default; Apple `AudioConverter` overrides with per-codec values.
    fn priming(&self, _codec: AudioCodec) -> CodecPriming {
        CodecPriming::default()
    }

    /// PCM output specification.
    fn spec(&self) -> PcmSpec;

    /// Codec-owned playback contract — currently the captured
    /// [`crate::GaplessInfo`] (encoder priming + trailing padding in
    /// PCM frames). Default returns the empty contract; per-platform
    /// implementations override when their codec exposes
    /// priming numbers (Apple `kAudioConverterPrimeInfo`, Symphonia
    /// `AudioDecoderOptions::gapless`, Android via the demuxer's
    /// container probe).
    ///
    /// `ComposedDecoder<D, C>` forwards this through
    /// [`crate::Decoder::track_info`] so the audio pipeline can build
    /// a [`crate::GaplessTrimmer`] without knowing the concrete codec
    /// type.
    fn track_info(&self) -> crate::DecoderTrackInfo {
        crate::DecoderTrackInfo::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codec_priming_default_is_all_zero() {
        let p = CodecPriming::default();
        assert_eq!(p.frames, 0);
        assert_eq!(p.packets, 0);
        assert_eq!(p.byte_margin, 0);
    }
}

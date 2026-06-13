use std::io::{Read, SeekFrom};

use kithara_stream::AudioCodec;

use super::{GaplessInfo, mp3::read_lame_trim, mp4::probe_mp4_gapless_dyn};
use crate::traits::DecoderInput;

/// Rewind `source` to byte 0, run [`probe_codec_gapless`], then rewind
/// again so the caller can hand the same source to a demuxer.
/// Replaces three near-identical "seek → probe → seek" blocks that
/// used to live in the Apple / Symphonia factory dispatch paths and
/// silently swallowed seek failures.
///
/// # Errors
///
/// Returns [`crate::error::DecodeError`] when the rewind seek fails —
/// previously a `let _ = source.seek(...)` bypass would have let the
/// demuxer read from mid-file.
pub(crate) fn scoped_probe(
    source: &mut dyn DecoderInput,
    codec: AudioCodec,
) -> crate::error::DecodeResult<Option<GaplessInfo>> {
    source.seek(SeekFrom::Start(0))?;
    let info = probe_codec_gapless(codec, source);
    source.seek(SeekFrom::Start(0))?;
    Ok(info)
}

/// Probe ENCODER-side priming/padding for one codec from the
/// underlying source. Returns `Some` only when real encoder metadata
/// exists (MP4 `udta`/`iTunSMPB`/`elst` for AAC, Xing/Info+LAME for
/// MP3); `None` otherwise. Decoder-side algorithmic delay is added by
/// each [`crate::codec::FrameCodec`] impl separately. See
/// `kithara-decode/README.md` "Gapless probe contract" for the full
/// per-backend table and empirical justification.
pub(crate) fn probe_codec_gapless(
    codec: AudioCodec,
    source: &mut dyn DecoderInput,
) -> Option<GaplessInfo> {
    /// LAME header probe window: read up to ~16 `KiB` to cover `ID3v2`
    /// tags and a couple of MP3 frames before the Xing/Info+LAME slot.
    const LAME_PROBE_WINDOW_BYTES: usize = 16 * 1024;
    match codec {
        AudioCodec::AacLc | AudioCodec::AacHe | AudioCodec::AacHeV2 => {
            probe_mp4_gapless_dyn(source).ok().flatten()
        }
        AudioCodec::Mp3 => {
            let mut buffer = Vec::with_capacity(LAME_PROBE_WINDOW_BYTES);
            source
                .take(LAME_PROBE_WINDOW_BYTES as u64)
                .read_to_end(&mut buffer)
                .ok()?;
            read_lame_trim(&buffer).map(|trim| GaplessInfo {
                leading_frames: u64::from(trim.enc_delay),
                trailing_frames: u64::from(trim.enc_padding),
            })
        }
        _ => None,
    }
}

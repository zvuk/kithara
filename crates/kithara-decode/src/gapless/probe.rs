use std::io::SeekFrom;

#[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
use kithara_platform::time::Duration;
use kithara_stream::AudioCodec;

#[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
use super::mp3::read_xing_duration;
use super::{GaplessInfo, mp3::read_lame_trim, mp4::probe_mp4_gapless_dyn};
use crate::traits::{DecoderInput, InputReadOutcome};

#[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
#[derive(Default)]
pub(crate) struct StartupProbe {
    pub(crate) duration: Option<Duration>,
    pub(crate) gapless: Option<GaplessInfo>,
}

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

/// Rewind `source`, read only the startup prefix needed for metadata that
/// must be available before decode startup, then rewind again for the demuxer.
#[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
pub(crate) fn scoped_startup_probe(
    source: &mut dyn DecoderInput,
    codec: AudioCodec,
) -> crate::error::DecodeResult<StartupProbe> {
    if !matches!(codec, AudioCodec::Mp3) {
        return Ok(StartupProbe::default());
    }

    source.seek(SeekFrom::Start(0))?;
    let buffer = read_mp3_probe_prefix(source);
    source.seek(SeekFrom::Start(0))?;

    let gapless = read_lame_trim(&buffer).map(|trim| GaplessInfo {
        leading_frames: u64::from(trim.enc_delay),
        trailing_frames: u64::from(trim.enc_padding),
    });
    let duration = read_xing_duration(&buffer);
    Ok(StartupProbe { duration, gapless })
}

/// Probe ENCODER-side priming/padding for one codec from the
/// underlying source. Returns `Some` only when real encoder metadata
/// exists (MP4 `udta`/`iTunSMPB`/`elst` for AAC, Xing/Info+LAME for
/// MP3); `None` otherwise. Decoder-side algorithmic delay is added by
/// each [`crate::codec::FrameCodec`] impl separately. See
/// `kithara-decode/CONTEXT.md` "Gapless probe contract" for the full
/// per-backend table and empirical justification.
pub(crate) fn probe_codec_gapless(
    codec: AudioCodec,
    source: &mut dyn DecoderInput,
) -> Option<GaplessInfo> {
    match codec {
        AudioCodec::AacLc | AudioCodec::AacHe | AudioCodec::AacHeV2 => {
            probe_mp4_gapless_dyn(source).ok().flatten()
        }
        AudioCodec::Mp3 => {
            let buffer = read_mp3_probe_prefix(source);
            read_lame_trim(&buffer).map(|trim| GaplessInfo {
                leading_frames: u64::from(trim.enc_delay),
                trailing_frames: u64::from(trim.enc_padding),
            })
        }
        _ => None,
    }
}

fn read_mp3_probe_prefix(source: &mut dyn DecoderInput) -> Vec<u8> {
    const WINDOW_BYTES: usize = 16 * 1024;
    const CHUNK_BYTES: usize = 1024;

    let mut buffer = Vec::with_capacity(WINDOW_BYTES);
    let mut scratch = [0u8; CHUNK_BYTES];
    while buffer.len() < WINDOW_BYTES {
        let remaining = WINDOW_BYTES - buffer.len();
        let want = remaining.min(scratch.len());
        match source.try_read(&mut scratch[..want]) {
            Ok(InputReadOutcome::Bytes(n)) => {
                buffer.extend_from_slice(&scratch[..n.get()]);
            }
            Ok(InputReadOutcome::Pending(_) | InputReadOutcome::Eof) | Err(_) => break,
        }
    }
    buffer
}

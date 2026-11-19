use std::io::SeekFrom;

use kithara_bufpool::{ByteBuffer, HasPool, PoolRegion};
use kithara_stream::AudioCodec;

use super::{
    GaplessInfo,
    mp3::{consts as mp3_consts, read_lame_trim, skip_id3v2},
    mp4::probe_mp4_gapless_dyn,
};
use crate::traits::{DecoderInput, InputReadOutcome};

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
pub(crate) fn scoped_probe<S>(
    source: &mut dyn DecoderInput,
    codec: AudioCodec,
    pools: &PoolRegion<S>,
) -> crate::error::DecodeResult<Option<GaplessInfo>>
where
    S: HasPool<u8>,
{
    source.seek(SeekFrom::Start(0))?;
    let info = probe_codec_gapless(codec, source, pools)?;
    source.seek(SeekFrom::Start(0))?;
    Ok(info)
}

/// Probe ENCODER-side priming/padding for one codec from the underlying source. Returns
/// `Some` only when real encoder metadata exists (MP4 `udta`/`iTunSMPB`/`elst` for AAC,
/// Xing/Info+LAME for MP3); `None` otherwise. Decoder-side algorithmic delay is added
/// by each [`crate::codec::FrameCodec`] impl separately.
pub(crate) fn probe_codec_gapless<S>(
    codec: AudioCodec,
    source: &mut dyn DecoderInput,
    pools: &PoolRegion<S>,
) -> crate::error::DecodeResult<Option<GaplessInfo>>
where
    S: HasPool<u8>,
{
    match codec {
        AudioCodec::AacLc | AudioCodec::AacHe | AudioCodec::AacHeV2 => {
            match probe_mp4_gapless_dyn(source, pools) {
                Ok(info) => Ok(info),
                Err(error @ crate::DecodeError::Pool { .. }) => Err(error),
                Err(_) => Ok(None),
            }
        }
        AudioCodec::Mp3 => {
            let buffer = read_mp3_probe_prefix(source, pools)?;
            Ok(read_lame_trim(&buffer).map(|trim| GaplessInfo {
                leading_frames: u64::from(trim.enc_delay),
                trailing_frames: u64::from(trim.enc_padding),
            }))
        }
        _ => Ok(None),
    }
}

/// Leading `ID3v2` tags can push the LAME fields past the probe window.
/// Seek past tags until the window holds those fields without buffering cover art.
fn read_mp3_probe_prefix<S>(
    source: &mut dyn DecoderInput,
    pools: &PoolRegion<S>,
) -> crate::error::DecodeResult<ByteBuffer>
where
    S: HasPool<u8>,
{
    let mut offset = 0_u64;
    loop {
        let buffer = read_probe_window(source, pools)?;
        let audio_start = skip_id3v2(&buffer);
        if buffer.len() < consts::WINDOW_BYTES
            || buffer.len().saturating_sub(audio_start) >= mp3_consts::LAME_TRIM_SPAN
        {
            return Ok(buffer);
        }

        let Some(next_offset) = u64::try_from(audio_start)
            .ok()
            .and_then(|length| offset.checked_add(length))
        else {
            return Ok(buffer);
        };
        if source.seek(SeekFrom::Start(next_offset)).is_err() {
            return Ok(buffer);
        }
        offset = next_offset;
    }
}

mod consts {
    pub(super) const CHUNK_BYTES: usize = 1024;
    pub(super) const WINDOW_BYTES: usize = 16 * 1024;
}

fn read_probe_window<S>(
    source: &mut dyn DecoderInput,
    pools: &PoolRegion<S>,
) -> crate::error::DecodeResult<ByteBuffer>
where
    S: HasPool<u8>,
{
    const WINDOW_BYTES: usize = consts::WINDOW_BYTES;
    const CHUNK_BYTES: usize = consts::CHUNK_BYTES;

    let mut buffer = pools.get_with_len::<u8>(WINDOW_BYTES)?;
    buffer.clear();
    let mut scratch = [0u8; CHUNK_BYTES];
    while buffer.len() < WINDOW_BYTES {
        let remaining = WINDOW_BYTES - buffer.len();
        let want = remaining.min(scratch.len());
        match source.try_read(&mut scratch[..want]) {
            Ok(InputReadOutcome::Bytes(n)) => {
                buffer.try_extend_from_slice(&scratch[..n.get()])?;
            }
            Ok(InputReadOutcome::Pending(_) | InputReadOutcome::Eof) | Err(_) => break,
        }
    }
    Ok(buffer)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use kithara_test_utils::kithara;

    use super::*;
    use crate::test_pools::pools;

    /// An `ID3v2` tag of `payload_len` bytes followed by one MPEG sync word,
    /// so a probe that stops inside the tag yields no frame at all.
    fn mp3_behind_id3(payload_len: usize) -> Vec<u8> {
        let mut data = vec![0u8; 10 + payload_len];
        data[..3].copy_from_slice(b"ID3");
        data[3] = 3;
        let len = u32::try_from(payload_len).expect("test tag fits u32");
        data[6] = ((len >> 21) & 0x7f) as u8;
        data[7] = ((len >> 14) & 0x7f) as u8;
        data[8] = ((len >> 7) & 0x7f) as u8;
        data[9] = (len & 0x7f) as u8;
        data.extend_from_slice(&[0xff, 0xfb, 0x90, 0x00]);
        data
    }

    #[kithara::test(native, flash(false))]
    fn probe_window_stays_put_when_the_source_ends_inside_the_tag() {
        let mut data = mp3_behind_id3(32 * 1024);
        data.truncate(4 * 1024);
        let mut source = Cursor::new(data);

        let buffer = read_mp3_probe_prefix(&mut source, &pools()).expect("BUG: read probe prefix");

        assert_eq!(buffer.first().copied(), Some(b'I'));
        assert_eq!(buffer.len(), 4 * 1024);
    }

    #[kithara::test(native, flash(false))]
    fn probe_window_keeps_a_tag_that_fits_it() {
        let tag_bytes = 10 + 1024;
        let mut data = mp3_behind_id3(1024);
        data.resize(consts::WINDOW_BYTES + 1024, 0);
        let mut source = Cursor::new(data);

        let buffer = read_mp3_probe_prefix(&mut source, &pools()).expect("BUG: read probe prefix");

        assert_eq!(buffer.first().copied(), Some(b'I'));
        assert_eq!(buffer.get(tag_bytes).copied(), Some(0xff));
    }
}

//! In-memory fMP4 init / segment parser.
//!
//! Two entry points:
//! - [`parse_init`] consumes a `ftyp+moov` init blob and returns a
//!   reusable [`Fmp4InitInfo`] (codec, timescale, codec-specific config
//!   bytes).
//! - [`parse_segment_frames`] consumes a `(styp|moof)+mdat` media
//!   segment and yields per-frame `(decode_time, duration, byte_range)`
//!   tuples bound to the segment buffer.
//!
//! The segment parser is hand-rolled rather than `Mp4::read_bytes`
//! because HLS media segments don't carry `ftyp+moov` and `re_mp4`
//! refuses to parse them.

use std::io::{Cursor, Read, Seek, SeekFrom};

use kithara_stream::AudioCodec;
use re_mp4::{BoxHeader, BoxType, FourCC, MoofBox, Mp4, ReadBox, StsdBoxContent};

use crate::error::{DecodeError, DecodeResult};

const FOURCC_FLAC: u32 = 0x664c_6143; // "fLaC"

/// Codec-specific decoder config bytes carried in the init segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CodecConfig {
    /// AAC `AudioSpecificConfig` bytes (`ESDS` `DecoderSpecificInfo` body).
    Aac(Vec<u8>),
    /// FLAC `STREAMINFO` block payload (34 bytes, no metadata header).
    Flac(Vec<u8>),
}

impl CodecConfig {
    #[cfg(test)]
    pub(crate) fn as_bytes(&self) -> &[u8] {
        match self {
            Self::Aac(bytes) | Self::Flac(bytes) => bytes,
        }
    }
}

/// Parsed init segment. Holds everything a segment-level codec needs
/// to decode subsequent media segments.
#[derive(Debug, Clone)]
pub(crate) struct Fmp4InitInfo {
    pub(crate) codec: AudioCodec,
    pub(crate) config: CodecConfig,
    pub(crate) channels: u16,
    pub(crate) sample_rate: u32,
    pub(crate) timescale: u32,
    pub(crate) track_id: u32,
}

/// Per-frame view into a single media segment's buffer.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Fmp4Frame {
    /// Frame duration in ticks.
    pub(crate) duration: u32,
    /// Absolute decode time in `init.timescale` ticks.
    pub(crate) decode_time: u64,
    /// Offset of frame bytes inside the segment buffer.
    pub(crate) offset: usize,
    /// Frame byte size.
    pub(crate) size: usize,
}

/// Parse an `EXT-X-MAP` init segment.
pub(crate) fn parse_init(bytes: &[u8]) -> DecodeResult<Fmp4InitInfo> {
    let mp4 =
        Mp4::read_bytes(bytes).map_err(|e| DecodeError::InvalidData(format!("re_mp4: {e}")))?;

    // Find the audio track. fMP4 init segments for HLS audio carry a
    // single trak; we still iterate defensively.
    let track_box = mp4
        .moov
        .traks
        .iter()
        .find(|trak| {
            matches!(trak.mdia.minf.stbl.stsd.contents, StsdBoxContent::Mp4a(_))
                || matches!(
                    trak.mdia.minf.stbl.stsd.contents,
                    StsdBoxContent::Unknown(_)
                )
        })
        .ok_or_else(|| DecodeError::InvalidData("no audio trak in init segment".into()))?;

    let timescale = track_box.mdia.mdhd.timescale;
    let track_id = track_box.tkhd.track_id;

    let (codec, sample_rate, channels, config) = match &track_box.mdia.minf.stbl.stsd.contents {
        StsdBoxContent::Mp4a(mp4a) => {
            let sample_rate = u32::from(mp4a.samplerate.value());
            let channels = mp4a.channelcount;
            let asc = build_aac_asc(mp4a)?;
            (
                AudioCodec::AacLc,
                sample_rate,
                channels,
                CodecConfig::Aac(asc),
            )
        }
        StsdBoxContent::Unknown(fourcc) if u32::from(*fourcc) == FOURCC_FLAC => {
            let (sample_rate, channels, streaminfo) = parse_flac_sample_entry(bytes)?;
            (
                AudioCodec::Flac,
                sample_rate,
                channels,
                CodecConfig::Flac(streaminfo),
            )
        }
        other => {
            return Err(DecodeError::InvalidData(format!(
                "unsupported audio sample entry: {other:?}"
            )));
        }
    };

    Ok(Fmp4InitInfo {
        codec,
        config,
        channels,
        sample_rate,
        timescale,
        track_id,
    })
}

/// Reconstruct the canonical 2-byte (AAC-LC) or 5-byte (HE-AAC)
/// `AudioSpecificConfig` from `re_mp4`'s parsed descriptor fields.
///
/// `re_mp4` throws away the raw `esds` bytes during parse, so we rebuild
/// the ASC layout described in ISO 14496-3 §1.6.2.1.
fn build_aac_asc(mp4a: &re_mp4::Mp4aBox) -> DecodeResult<Vec<u8>> {
    let esds = mp4a
        .esds
        .as_ref()
        .ok_or_else(|| DecodeError::InvalidData("mp4a missing esds".into()))?;
    let dec = &esds.es_desc.dec_config.dec_specific;
    let profile = dec.profile;
    let freq_index = dec.freq_index;
    let chan_conf = dec.chan_conf;

    if profile == 0 {
        return Err(DecodeError::InvalidData("invalid AAC profile=0".into()));
    }

    // Standard 2-byte ASC (profile <= 31, freq_index <= 14).
    if profile <= 31 && freq_index <= 14 {
        let byte0 = (profile << 3) | (freq_index >> 1);
        let byte1 = ((freq_index & 0x01) << 7) | (chan_conf << 3);
        return Ok(vec![byte0, byte1]);
    }

    Err(DecodeError::InvalidData(format!(
        "extended AAC profile not supported yet (profile={profile}, freq_index={freq_index})"
    )))
}

/// Locate `fLaC` sample entry inside the init bytes and read its
/// associated `dfLa` box payload (FLAC STREAMINFO).
fn parse_flac_sample_entry(bytes: &[u8]) -> DecodeResult<(u32, u16, Vec<u8>)> {
    const FOURCC_DFLA: u32 = 0x6466_4c61; // "dfLa"

    let mut cursor = Cursor::new(bytes);
    let total = bytes.len() as u64;

    // Walk to moov → trak → mdia → minf → stbl → stsd → fLaC.
    descend_into(&mut cursor, total, BoxType::MoovBox)?;
    let moov_end = cursor.position() + read_box_size(&mut cursor)? - 8;
    descend_into(&mut cursor, moov_end, BoxType::TrakBox)?;
    let trak_end = cursor.position() + read_box_size(&mut cursor)? - 8;
    descend_into(&mut cursor, trak_end, BoxType::MdiaBox)?;
    let mdia_end = cursor.position() + read_box_size(&mut cursor)? - 8;
    descend_into(&mut cursor, mdia_end, BoxType::MinfBox)?;
    let minf_end = cursor.position() + read_box_size(&mut cursor)? - 8;
    descend_into(&mut cursor, minf_end, BoxType::StblBox)?;
    let stbl_end = cursor.position() + read_box_size(&mut cursor)? - 8;
    descend_into(&mut cursor, stbl_end, BoxType::StsdBox)?;
    let stsd_size = read_box_size(&mut cursor)?;
    let stsd_end = cursor.position() + stsd_size - 8;

    // stsd FullBox header (4) + entry_count (4).
    cursor
        .seek(SeekFrom::Current(8))
        .map_err(|e| DecodeError::InvalidData(format!("seek past stsd header: {e}")))?;

    // First sample entry header — should be `fLaC`.
    let entry_start = cursor.position();
    let (entry_type, entry_size) = read_header(&mut cursor)?;
    if u32::from(entry_type) != FOURCC_FLAC {
        return Err(DecodeError::InvalidData(format!(
            "expected fLaC sample entry, found {:?}",
            FourCC::from(entry_type).value
        )));
    }
    let entry_end = entry_start + entry_size;

    // SoundSampleEntry: 6 reserved + 2 data_reference_index +
    // 8 reserved + 2 channel_count + 2 sample_size + 4 reserved +
    // 4 sample_rate (16.16 fixed).
    cursor
        .seek(SeekFrom::Current(8))
        .map_err(|e| DecodeError::InvalidData(format!("seek past sample entry header: {e}")))?;
    let mut buf = [0u8; 20];
    cursor
        .read_exact(&mut buf)
        .map_err(|e| DecodeError::InvalidData(format!("read sample entry: {e}")))?;
    let channels = u16::from_be_bytes([buf[8], buf[9]]);
    let sample_rate_raw = u32::from_be_bytes([buf[16], buf[17], buf[18], buf[19]]);
    let sample_rate = sample_rate_raw >> 16;

    // Walk children of the fLaC sample entry until we find dfLa.
    while cursor.position() < entry_end {
        let inner_start = cursor.position();
        let (inner_type, inner_size) = read_header(&mut cursor)?;
        if u32::from(inner_type) == FOURCC_DFLA {
            // dfLa layout per ISO 14496-12 + FLAC-in-ISOBMFF spec:
            //   8 bytes: box header (size + 'dfLa')
            //   4 bytes: FullBox version+flags
            //   4 bytes: METADATA_BLOCK_HEADER (block type 0 + length 34)
            //  34 bytes: STREAMINFO body
            // Symphonia's FLAC decoder takes `extra_data` = STREAMINFO
            // body (34 bytes), without the metadata block header.
            cursor
                .seek(SeekFrom::Current(4 + 4))
                .map_err(|e| DecodeError::InvalidData(format!("seek past dfLa header: {e}")))?;
            let mut payload = vec![0u8; 34];
            cursor
                .read_exact(&mut payload)
                .map_err(|e| DecodeError::InvalidData(format!("read STREAMINFO: {e}")))?;
            let _ = inner_size;
            return Ok((sample_rate, channels, payload));
        }
        cursor
            .seek(SeekFrom::Start(inner_start + inner_size))
            .map_err(|e| DecodeError::InvalidData(format!("skip sample entry child: {e}")))?;
    }
    let _ = stsd_end;
    Err(DecodeError::InvalidData("dfLa box not found".into()))
}

fn descend_into(cursor: &mut Cursor<&[u8]>, end: u64, target: BoxType) -> DecodeResult<()> {
    while cursor.position() < end {
        let pos = cursor.position();
        let (box_type, size) = read_header(cursor)?;
        if box_type == target {
            cursor
                .seek(SeekFrom::Start(pos))
                .map_err(|e| DecodeError::InvalidData(format!("rewind to box header: {e}")))?;
            return Ok(());
        }
        cursor
            .seek(SeekFrom::Start(pos + size))
            .map_err(|e| DecodeError::InvalidData(format!("skip box: {e}")))?;
    }
    Err(DecodeError::InvalidData(format!(
        "box {target:?} not found"
    )))
}

fn read_header(cursor: &mut Cursor<&[u8]>) -> DecodeResult<(BoxType, u64)> {
    let header =
        BoxHeader::read(cursor).map_err(|e| DecodeError::InvalidData(format!("re_mp4: {e}")))?;
    Ok((header.name, header.size))
}

fn read_box_size(cursor: &mut Cursor<&[u8]>) -> DecodeResult<u64> {
    // Caller has already stepped *past* the 8-byte header during a
    // previous `read_header`; we want the size of the *next* box. The
    // helper rewinds, reads, then leaves the cursor positioned right
    // after the header (8 bytes consumed) so descend_into can resume.
    let pos = cursor.position();
    let (_, size) = read_header(cursor)?;
    let _ = pos;
    Ok(size)
}

/// Walk a media segment's `(moof, mdat)` pairs and emit per-frame
/// descriptors. The returned offsets are relative to `segment_bytes`.
pub(crate) fn parse_segment_frames(
    init: &Fmp4InitInfo,
    segment_bytes: &[u8],
) -> DecodeResult<Vec<Fmp4Frame>> {
    let total = segment_bytes.len() as u64;
    let mut cursor = Cursor::new(segment_bytes);
    let mut frames = Vec::new();

    while cursor.position() < total {
        let box_start = cursor.position();
        let (box_type, size) = read_header(&mut cursor)?;
        if size < 8 {
            return Err(DecodeError::InvalidData(format!(
                "invalid box size {size} at offset {box_start}"
            )));
        }
        let box_end = box_start + size;

        if box_type == BoxType::MoofBox {
            let moof = MoofBox::read_box(&mut cursor, size)
                .map_err(|e| DecodeError::InvalidData(format!("re_mp4: {e}")))?;
            // Position cursor at the byte right after the moof. The
            // very next box in HLS segments is mdat carrying sample
            // data — `tfhd::base_data_offset` is rare; the typical
            // shape uses `default_base_is_moof + trun.data_offset`
            // counted relative to the moof start.
            cursor
                .seek(SeekFrom::Start(box_end))
                .map_err(|e| DecodeError::InvalidData(format!("seek after moof: {e}")))?;

            let mdat_start = cursor.position();
            let (mdat_type, mdat_size) = read_header(&mut cursor)?;
            if mdat_type != BoxType::MdatBox {
                return Err(DecodeError::InvalidData(format!(
                    "expected mdat after moof, got {mdat_type:?}"
                )));
            }
            let mdat_payload_start = cursor.position();
            // Fast-forward past the mdat payload for the next loop iteration.
            cursor
                .seek(SeekFrom::Start(mdat_start + mdat_size))
                .map_err(|e| DecodeError::InvalidData(format!("seek past mdat: {e}")))?;

            collect_frames(
                init,
                &moof,
                box_start,
                mdat_payload_start,
                segment_bytes,
                &mut frames,
            )?;
            continue;
        }

        // styp / sidx / free / etc.: skip.
        cursor
            .seek(SeekFrom::Start(box_end))
            .map_err(|e| DecodeError::InvalidData(format!("skip top-level box: {e}")))?;
    }

    Ok(frames)
}

fn collect_frames(
    init: &Fmp4InitInfo,
    moof: &MoofBox,
    moof_start: u64,
    mdat_payload_start: u64,
    segment_bytes: &[u8],
    out: &mut Vec<Fmp4Frame>,
) -> DecodeResult<()> {
    let traf = moof
        .trafs
        .iter()
        .find(|t| t.tfhd.track_id == init.track_id)
        .or_else(|| moof.trafs.first())
        .ok_or_else(|| DecodeError::InvalidData("moof has no traf".into()))?;

    let tfhd = &traf.tfhd;
    let default_base_is_moof = (tfhd.flags & re_mp4::TfhdBox::FLAG_DEFAULT_BASE_IS_MOOF) != 0;
    let mut decode_time = traf.tfdt.as_ref().map_or(0, |t| t.base_media_decode_time);

    for trun in &traf.truns {
        let data_offset_i32 = trun.data_offset.unwrap_or(0);
        // base = moof_start when default_base_is_moof; otherwise it's
        // the explicit tfhd.base_data_offset, falling back to moof
        // start (the modal HLS shape).
        let base = if default_base_is_moof {
            moof_start
        } else {
            tfhd.base_data_offset.unwrap_or(moof_start)
        };
        let mut byte_cursor = if data_offset_i32 < 0 {
            base.saturating_sub(u64::from(data_offset_i32.unsigned_abs()))
        } else {
            base.saturating_add(u64::try_from(data_offset_i32).unwrap_or(0))
        };
        let _ = mdat_payload_start;

        for sample_idx in 0..trun.sample_count as usize {
            let size = sample_size_for(trun, tfhd, sample_idx)?;
            let duration = sample_duration_for(trun, tfhd, sample_idx);

            let start = usize::try_from(byte_cursor).map_err(|_| {
                DecodeError::InvalidData(format!("frame offset overflows usize: {byte_cursor}"))
            })?;
            let end = start
                .checked_add(size as usize)
                .ok_or_else(|| DecodeError::InvalidData("sample byte range overflow".into()))?;
            if end > segment_bytes.len() {
                return Err(DecodeError::InvalidData(format!(
                    "sample byte range {start}..{end} past segment end {}",
                    segment_bytes.len()
                )));
            }

            out.push(Fmp4Frame {
                offset: start,
                size: size as usize,
                decode_time,
                duration,
            });

            byte_cursor = byte_cursor.saturating_add(u64::from(size));
            decode_time = decode_time.saturating_add(u64::from(duration));
        }
    }

    Ok(())
}

fn sample_size_for(
    trun: &re_mp4::TrunBox,
    tfhd: &re_mp4::TfhdBox,
    idx: usize,
) -> DecodeResult<u32> {
    if (trun.flags & re_mp4::TrunBox::FLAG_SAMPLE_SIZE) != 0 {
        return trun
            .sample_sizes
            .get(idx)
            .copied()
            .ok_or_else(|| DecodeError::InvalidData("missing trun sample_size".into()));
    }
    tfhd.default_sample_size
        .ok_or_else(|| DecodeError::InvalidData("no default_sample_size".into()))
}

fn sample_duration_for(trun: &re_mp4::TrunBox, tfhd: &re_mp4::TfhdBox, idx: usize) -> u32 {
    if (trun.flags & re_mp4::TrunBox::FLAG_SAMPLE_DURATION) != 0 {
        return trun.sample_durations.get(idx).copied().unwrap_or(0);
    }
    tfhd.default_sample_duration.unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use kithara_test_utils::kithara;

    use super::*;

    fn read_fixture(name: &str) -> Vec<u8> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../assets/hls")
            .join(name);
        std::fs::read(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"))
    }

    #[kithara::test]
    fn parse_init_aac_extracts_codec_and_asc() {
        let bytes = read_fixture("init-slq-a1.mp4");
        let init = parse_init(&bytes).expect("parse init");
        assert_eq!(init.codec, AudioCodec::AacLc);
        assert!(init.timescale > 0, "timescale={}", init.timescale);
        assert!(init.sample_rate >= 8_000 && init.sample_rate <= 96_000);
        assert!(init.channels >= 1 && init.channels <= 8);
        let asc = init.config.as_bytes();
        assert!(
            asc.len() == 2 || asc.len() == 5,
            "ASC length unexpected: {} bytes",
            asc.len()
        );
        // ASC byte0 high 5 bits = audio object type. AAC-LC = 2.
        let aot = asc[0] >> 3;
        assert_eq!(aot, 2, "expected AAC-LC AOT=2, got {aot}");
    }

    #[kithara::test]
    fn parse_init_flac_extracts_streaminfo() {
        let bytes = read_fixture("init-slossless-a1.mp4");
        let init = parse_init(&bytes).expect("parse FLAC init");
        assert_eq!(init.codec, AudioCodec::Flac);
        assert!(matches!(init.config, CodecConfig::Flac(_)));
        // STREAMINFO body is exactly 34 bytes per RFC 9639. Symphonia's
        // FLAC decoder takes the body without the 4-byte metadata block
        // header.
        let len = init.config.as_bytes().len();
        assert_eq!(len, 34, "STREAMINFO body must be 34 bytes");
    }

    #[kithara::test]
    fn parse_segment_frames_aac_yields_monotonic_frames() {
        let init_bytes = read_fixture("init-slq-a1.mp4");
        let init = parse_init(&init_bytes).expect("parse init");
        let seg_bytes = read_fixture("segment-1-slq-a1.m4s");
        let frames = parse_segment_frames(&init, &seg_bytes).expect("parse seg");
        assert!(
            frames.len() > 40,
            "expected ≥40 frames, got {}",
            frames.len()
        );

        // Decode time strictly monotonic across consecutive frames.
        for pair in frames.windows(2) {
            let (a, b) = (&pair[0], &pair[1]);
            assert!(
                b.decode_time > a.decode_time,
                "non-monotonic decode_time: {} -> {}",
                a.decode_time,
                b.decode_time
            );
        }
        // Each frame's byte range must lie inside the segment buffer.
        for f in &frames {
            assert!(
                f.offset + f.size <= seg_bytes.len(),
                "frame {}+{} > seg {}",
                f.offset,
                f.size,
                seg_bytes.len()
            );
            assert!(f.size > 0);
        }
    }

    #[kithara::test]
    fn parse_segment_frames_total_duration_matches_extinf() {
        let init_bytes = read_fixture("init-slq-a1.mp4");
        let init = parse_init(&init_bytes).expect("parse init");
        let seg_bytes = read_fixture("segment-1-slq-a1.m4s");
        let frames = parse_segment_frames(&init, &seg_bytes).expect("parse seg");
        let total_ticks: u64 = frames.iter().map(|f| u64::from(f.duration)).sum();
        let total_seconds =
            Duration::from_nanos(total_ticks * 1_000_000_000 / u64::from(init.timescale))
                .as_secs_f64();
        // Test fixture segments are ~6s (allow small drift for last-frame trim).
        assert!(
            total_seconds > 5.0 && total_seconds < 7.0,
            "segment duration off: {total_seconds}s (timescale={})",
            init.timescale
        );
    }
}

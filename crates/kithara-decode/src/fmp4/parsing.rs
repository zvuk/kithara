use std::io::{self, Cursor, Error, Read, Seek, SeekFrom};

use kithara_bufpool::{HasPool, PoolRegion};
use kithara_stream::AudioCodec;
use re_mp4::{BoxHeader, BoxType, Mp4, StsdBoxContent};

use crate::{
    consts,
    error::{DecodeError, DecodeResult},
};

/// Codec-specific decoder config bytes carried in the init segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CodecConfig {
    /// AAC `AudioSpecificConfig` bytes (`ESDS` `DecoderSpecificInfo` body).
    Aac(Vec<u8>),
    /// FLAC `STREAMINFO` block payload (34 bytes, no metadata header).
    Flac([u8; consts::FLAC_STREAMINFO_BYTES]),
}

impl AsRef<[u8]> for CodecConfig {
    fn as_ref(&self) -> &[u8] {
        match self {
            Self::Aac(bytes) => bytes,
            Self::Flac(bytes) => bytes,
        }
    }
}

/// Parsed init segment. Holds everything a segment-level codec needs
/// to decode subsequent media segments.
#[derive(Debug, Clone)]
pub(crate) struct Fmp4InitInfo {
    pub(crate) codec: AudioCodec,
    pub(crate) config: CodecConfig,
    /// Container-level gapless info derived from the init segment
    /// (`elst` edit-list trim or `udta` `iTunSMPB`). `None` when the
    /// init blob carries neither — codec-side capture (Apple `PrimeInfo`
    /// refresh) supplements this when the codec exposes priming.
    pub(crate) gapless: Option<crate::GaplessInfo>,
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
pub(crate) fn parse_init<S>(bytes: &[u8], pools: &PoolRegion<S>) -> DecodeResult<Fmp4InitInfo>
where
    S: HasPool<u8>,
{
    let mp4 = Mp4::read_bytes(bytes).map_err(|e| DecodeError::parse("re_mp4", e))?;

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
        .ok_or_else(|| DecodeError::InvalidData {
            detail: "no audio trak in init segment",
        })?;

    let timescale = track_box.mdia.mdhd.timescale;
    let track_id = track_box.tkhd.track_id;

    let (codec, sample_rate, channels, config) = match &track_box.mdia.minf.stbl.stsd.contents {
        StsdBoxContent::Mp4a(mp4a) => {
            let sample_rate = u32::from(mp4a.samplerate.value());
            let channels = mp4a.channelcount;
            let asc = extract_aac_asc_raw(bytes)?;
            (
                AudioCodec::AacLc,
                sample_rate,
                channels,
                CodecConfig::Aac(asc),
            )
        }
        StsdBoxContent::Unknown(fourcc) if u32::from(*fourcc) == consts::FOURCC_FLAC => {
            let (sample_rate, channels, streaminfo) = parse_flac_sample_entry(bytes)?;
            (
                AudioCodec::Flac,
                sample_rate,
                channels,
                CodecConfig::Flac(streaminfo),
            )
        }
        _ => {
            return Err(DecodeError::InvalidData {
                detail: "unsupported audio sample entry",
            });
        }
    };

    let gapless = {
        let mut cursor = Cursor::new(bytes);
        crate::gapless::probe_mp4_gapless(&mut cursor, pools).unwrap_or(None)
    };

    Ok(Fmp4InitInfo {
        codec,
        config,
        gapless,
        channels,
        sample_rate,
        timescale,
        track_id,
    })
}

/// Locate the `mp4a` sample entry inside the init bytes and pull the
/// raw `DecoderSpecificInfo` (descriptor tag 0x05) bytes out of its
/// `esds` box.
///
/// `re_mp4` exposes the descriptor only as three parsed fields
/// (profile / `freq_index` / `chan_conf`) and discards the rest, so
/// for HE-AAC v1/v2 with explicit AOT-29 signalling — which encodes
/// extension-AOT, extension sample-rate index, and (for PS) a PS
/// presence flag in bytes 3+ — a reconstruction from those three
/// fields drops everything past byte 2 and ends with fdk-aac
/// rejecting the config as "unexpected end of bitstream". This path
/// walks the boxes manually, finds the `esds` payload, decodes the
/// MPEG-4 `SLConfigDescriptor` / `ESDescriptor` / `DecoderConfigDescriptor`
/// / `DecoderSpecificInfo` descriptor chain by tag, and returns the
/// DSI body verbatim.
fn extract_aac_asc_raw(bytes: &[u8]) -> DecodeResult<Vec<u8>> {
    const FOURCC_MP4A: u32 = 0x6d70_3461;
    const FOURCC_ESDS: u32 = 0x6573_6473;

    let mut cursor = Cursor::new(bytes);
    let total = bytes.len() as u64;

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

    cursor
        .seek(SeekFrom::Current(8))
        .map_err(|e| DecodeError::parse("seek past stsd header", e))?;

    let entry_start = cursor.position();
    let (entry_type, entry_size) = read_header(&mut cursor)?;
    if u32::from(entry_type) != FOURCC_MP4A {
        return Err(DecodeError::InvalidData {
            detail: "expected mp4a sample entry",
        });
    }
    let entry_end = entry_start + entry_size;
    let _ = stsd_end;

    cursor
        .seek(SeekFrom::Current(28))
        .map_err(|e| DecodeError::parse("seek past mp4a header", e))?;

    while cursor.position() < entry_end {
        let child_start = cursor.position();
        let (child_type, child_size) = read_header(&mut cursor)?;
        if u32::from(child_type) == FOURCC_ESDS {
            cursor
                .seek(SeekFrom::Current(4))
                .map_err(|e| DecodeError::parse("seek past esds header", e))?;
            return read_esds_decoder_specific_info(&mut cursor, child_start + child_size);
        }
        cursor
            .seek(SeekFrom::Start(child_start + child_size))
            .map_err(|e| DecodeError::parse("skip mp4a child", e))?;
    }
    Err(DecodeError::InvalidData {
        detail: "esds box not found",
    })
}

/// Walk the descriptor chain `ES_Descriptor` → `DecoderConfigDescriptor`
/// → `DecoderSpecificInfo` inside an `esds` payload and return the
/// DSI body bytes. Each descriptor uses ISO/IEC 14496-1 tag+length
/// framing: a single-byte tag followed by an expandable-size big-endian
/// 7-bit-per-byte length (up to 4 bytes).
fn read_esds_decoder_specific_info(
    cursor: &mut Cursor<&[u8]>,
    esds_end: u64,
) -> DecodeResult<Vec<u8>> {
    const TAG_ES_DESCRIPTOR: u8 = 0x03;
    const TAG_DECODER_CONFIG: u8 = 0x04;
    const TAG_DECODER_SPECIFIC: u8 = 0x05;

    let (es_tag, es_size) = read_descriptor_header(cursor)?;
    if es_tag != TAG_ES_DESCRIPTOR {
        return Err(DecodeError::InvalidData {
            detail: "expected ES_Descriptor (0x03 tag)",
        });
    }
    let es_body_end = cursor.position() + u64::from(es_size);
    let mut header = [0u8; 3];
    cursor
        .read_exact(&mut header)
        .map_err(|e| DecodeError::parse("read ES_Descriptor header", e))?;
    let flags = header[2];
    if flags & 0x80 != 0 {
        cursor
            .seek(SeekFrom::Current(2))
            .map_err(|e| DecodeError::parse("skip dependsOn_ES_ID", e))?;
    }
    if flags & 0x40 != 0 {
        let mut url_len = [0u8; 1];
        cursor
            .read_exact(&mut url_len)
            .map_err(|e| DecodeError::parse("read URL_length", e))?;
        cursor
            .seek(SeekFrom::Current(i64::from(url_len[0])))
            .map_err(|e| DecodeError::parse("skip URL", e))?;
    }
    if flags & 0x20 != 0 {
        cursor
            .seek(SeekFrom::Current(2))
            .map_err(|e| DecodeError::parse("skip OCR_ES_ID", e))?;
    }

    let _ = es_body_end;
    let (dc_tag, dc_size) = read_descriptor_header(cursor)?;
    if dc_tag != TAG_DECODER_CONFIG {
        return Err(DecodeError::InvalidData {
            detail: "expected DecoderConfigDescriptor (0x04 tag)",
        });
    }
    let dc_end = cursor.position() + u64::from(dc_size);
    cursor
        .seek(SeekFrom::Current(13))
        .map_err(|e| DecodeError::parse("skip DCD body", e))?;

    let (dsi_tag, dsi_size) = read_descriptor_header(cursor)?;
    if dsi_tag != TAG_DECODER_SPECIFIC {
        return Err(DecodeError::InvalidData {
            detail: "expected DecoderSpecificInfo (0x05 tag)",
        });
    }
    if cursor.position() + u64::from(dsi_size) > dc_end.min(esds_end) {
        return Err(DecodeError::InvalidData {
            detail: "DSI extends past parent descriptor",
        });
    }
    let mut payload = vec![0u8; dsi_size as usize];
    cursor
        .read_exact(&mut payload)
        .map_err(|e| DecodeError::parse("read DSI body", e))?;
    Ok(payload)
}

/// MPEG-4 descriptor header: 1-byte tag + variable-length size (each
/// size byte's MSB is a continuation flag, low 7 bits feed the running
/// size value). Capped at 4 size bytes per the spec.
fn read_descriptor_header(cursor: &mut Cursor<&[u8]>) -> DecodeResult<(u8, u32)> {
    let mut tag = [0u8; 1];
    cursor
        .read_exact(&mut tag)
        .map_err(|e| DecodeError::parse("read descriptor tag", e))?;
    let mut size: u32 = 0;
    for _ in 0..4 {
        let mut b = [0u8; 1];
        cursor
            .read_exact(&mut b)
            .map_err(|e| DecodeError::parse("read descriptor size byte", e))?;
        size = (size << 7) | u32::from(b[0] & 0x7F);
        if b[0] & 0x80 == 0 {
            return Ok((tag[0], size));
        }
    }
    Err(DecodeError::InvalidData {
        detail: "descriptor size length exceeds 4 bytes",
    })
}

/// Locate `fLaC` sample entry inside the init bytes and read its
/// associated `dfLa` box payload (FLAC STREAMINFO).
fn parse_flac_sample_entry(
    bytes: &[u8],
) -> DecodeResult<(u32, u16, [u8; consts::FLAC_STREAMINFO_BYTES])> {
    const FOURCC_DFLA: u32 = 0x6466_4c61;

    let mut cursor = Cursor::new(bytes);
    let total = bytes.len() as u64;

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

    cursor
        .seek(SeekFrom::Current(8))
        .map_err(|e| DecodeError::parse("seek past stsd header", e))?;

    let entry_start = cursor.position();
    let (entry_type, entry_size) = read_header(&mut cursor)?;
    if u32::from(entry_type) != consts::FOURCC_FLAC {
        return Err(DecodeError::InvalidData {
            detail: "expected fLaC sample entry",
        });
    }
    let entry_end = entry_start + entry_size;

    cursor
        .seek(SeekFrom::Current(8))
        .map_err(|e| DecodeError::parse("seek past sample entry header", e))?;
    let mut buf = [0u8; 20];
    cursor
        .read_exact(&mut buf)
        .map_err(|e| DecodeError::parse("read sample entry", e))?;
    let channels = u16::from_be_bytes([buf[8], buf[9]]);
    let sample_rate_raw = u32::from_be_bytes([buf[16], buf[17], buf[18], buf[19]]);
    let sample_rate = sample_rate_raw >> 16;

    while cursor.position() < entry_end {
        let inner_start = cursor.position();
        let (inner_type, inner_size) = read_header(&mut cursor)?;
        if u32::from(inner_type) == FOURCC_DFLA {
            cursor
                .seek(SeekFrom::Current(4 + 4))
                .map_err(|e| DecodeError::parse("seek past dfLa header", e))?;
            let mut payload = [0u8; consts::FLAC_STREAMINFO_BYTES];
            cursor
                .read_exact(&mut payload)
                .map_err(|e| DecodeError::parse("read STREAMINFO", e))?;
            let _ = inner_size;
            return Ok((sample_rate, channels, payload));
        }
        cursor
            .seek(SeekFrom::Start(inner_start + inner_size))
            .map_err(|e| DecodeError::parse("skip sample entry child", e))?;
    }
    let _ = stsd_end;
    Err(DecodeError::InvalidData {
        detail: "dfLa box not found",
    })
}

fn descend_into(cursor: &mut Cursor<&[u8]>, end: u64, target: BoxType) -> DecodeResult<()> {
    while cursor.position() < end {
        let pos = cursor.position();
        let (box_type, size) = read_header(cursor)?;
        if box_type == target {
            cursor
                .seek(SeekFrom::Start(pos))
                .map_err(|e| DecodeError::parse("rewind to box header", e))?;
            return Ok(());
        }
        cursor
            .seek(SeekFrom::Start(pos + size))
            .map_err(|e| DecodeError::parse("skip box", e))?;
    }
    Err(DecodeError::InvalidData {
        detail: "target box not found",
    })
}

fn read_header(cursor: &mut Cursor<&[u8]>) -> DecodeResult<(BoxType, u64)> {
    let header = BoxHeader::read(cursor).map_err(|e| DecodeError::parse("re_mp4", e))?;
    Ok((header.name, header.size))
}

fn read_box_size(cursor: &mut Cursor<&[u8]>) -> DecodeResult<u64> {
    let pos = cursor.position();
    let (_, size) = read_header(cursor)?;
    let _ = pos;
    Ok(size)
}

/// Walk a media segment's `(moof, mdat)` pairs and emit per-frame
/// descriptors. The returned offsets are relative to `segment_bytes`.
///
/// The box walk itself belongs to `kithara-mp4`; what stays here is the
/// projection of its samples onto the buffer-relative view the demuxer
/// slices frames out of.
///
/// The frame vector is presized and filled by hand, since collecting into a `Result<Vec<_>>` would
/// lose the exact capacity, and a segment must cost exactly one allocation.
pub(crate) fn parse_segment_frames(
    init: &Fmp4InitInfo,
    segment_bytes: &[u8],
) -> DecodeResult<Vec<Fmp4Frame>> {
    let total = u64::try_from(segment_bytes.len()).map_err(|_| DecodeError::InvalidData {
        detail: "segment length overflows u64",
    })?;
    let samples = kithara_mp4::read_samples(&SegmentBytes(segment_bytes), total, init.track_id)
        .map_err(|error| DecodeError::InvalidData {
            detail: error.detail(),
        })?;
    let mut frames: Vec<Fmp4Frame> = Vec::with_capacity(samples.len());
    for sample in &samples {
        frames.push(frame_from_sample(sample)?);
    }
    Ok(frames)
}

/// Random-access view of one media segment already held in memory. The walk
/// takes a [`kithara_mp4::ReadAt`] because it is written for sources it must
/// not pull whole; a segment buffer simply answers from the slice it is.
struct SegmentBytes<'a>(&'a [u8]);

impl kithara_mp4::ReadAt for SegmentBytes<'_> {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        let start = usize::try_from(offset).map_err(Error::other)?;
        let Some(tail) = self.0.get(start..) else {
            return Ok(0);
        };
        let n = tail.len().min(buf.len());
        buf[..n].copy_from_slice(&tail[..n]);
        Ok(n)
    }
}

/// Project one walked sample onto the segment buffer the demuxer slices.
fn frame_from_sample(sample: &kithara_mp4::Sample) -> DecodeResult<Fmp4Frame> {
    let offset =
        usize::try_from(sample.byte_range.start).map_err(|_| DecodeError::InvalidData {
            detail: "frame offset overflows usize",
        })?;
    let size = usize::try_from(sample.byte_range.end - sample.byte_range.start).map_err(|_| {
        DecodeError::InvalidData {
            detail: "frame size overflows usize",
        }
    })?;
    Ok(Fmp4Frame {
        offset,
        size,
        decode_time: sample.decode_ticks,
        duration: sample.duration_ticks,
    })
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use kithara_platform::time::Duration;
    use kithara_test_fixtures::unit_fixtures::{aac_init, aac_segment, flac_init};
    use kithara_test_utils::kithara;

    use super::*;
    use crate::test_pools::pools;

    #[kithara::test]
    fn parse_init_aac_extracts_codec_and_asc(aac_init: Vec<u8>) {
        let bytes = aac_init;
        let init = parse_init(&bytes, &pools()).expect("BUG: parse init");
        assert_eq!(init.codec, AudioCodec::AacLc);
        assert!(init.timescale > 0, "timescale={}", init.timescale);
        assert!(init.sample_rate >= 8_000 && init.sample_rate <= 96_000);
        assert!(init.channels >= 1 && init.channels <= 8);
        let asc = init.config.as_ref();
        assert!(
            asc.len() == 2 || asc.len() == 5,
            "ASC length unexpected: {} bytes",
            asc.len()
        );
        let aot = asc[0] >> 3;
        assert_eq!(aot, 2, "expected AAC-LC AOT=2, got {aot}");
    }

    #[kithara::test]
    fn parse_init_flac_extracts_streaminfo(flac_init: Vec<u8>) {
        let bytes = flac_init;
        let init = parse_init(&bytes, &pools()).expect("BUG: parse FLAC init");
        assert_eq!(init.codec, AudioCodec::Flac);
        assert!(matches!(init.config, CodecConfig::Flac(_)));
        let len = init.config.as_ref().len();
        assert_eq!(len, 34, "STREAMINFO body must be 34 bytes");
    }

    #[kithara::test]
    fn parse_segment_frames_aac_yields_monotonic_frames(aac_init: Vec<u8>, aac_segment: Vec<u8>) {
        let init_bytes = aac_init;
        let init = parse_init(&init_bytes, &pools()).expect("BUG: parse init");
        let seg_bytes = aac_segment;
        let frames = parse_segment_frames(&init, &seg_bytes).expect("BUG: parse seg");
        assert!(
            frames.len() > 40,
            "expected ≥40 frames, got {}",
            frames.len()
        );

        for pair in frames.windows(2) {
            let (a, b) = (&pair[0], &pair[1]);
            assert!(
                b.decode_time > a.decode_time,
                "non-monotonic decode_time: {} -> {}",
                a.decode_time,
                b.decode_time
            );
        }
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

    /// R-remp4: the per-frame `Vec<Fmp4Frame>` must be presized from the
    /// `trun` sample count, so a single-moof segment is built with exactly
    /// one allocation — capacity equals the frame count, no realloc churn.
    #[kithara::test]
    fn parse_segment_frames_presizes_vec_to_sample_count(aac_init: Vec<u8>, aac_segment: Vec<u8>) {
        let init_bytes = aac_init;
        let init = parse_init(&init_bytes, &pools()).expect("BUG: parse init");
        let seg_bytes = aac_segment;
        let frames = parse_segment_frames(&init, &seg_bytes).expect("BUG: parse seg");
        assert!(!frames.is_empty(), "segment must yield frames");
        assert_eq!(
            frames.capacity(),
            frames.len(),
            "Vec<Fmp4Frame> must be presized to the trun sample count \
             (exact capacity, single allocation)",
        );
    }

    #[kithara::test]
    fn parse_segment_frames_total_duration_matches_extinf(aac_init: Vec<u8>, aac_segment: Vec<u8>) {
        let init_bytes = aac_init;
        let init = parse_init(&init_bytes, &pools()).expect("BUG: parse init");
        let seg_bytes = aac_segment;
        let frames = parse_segment_frames(&init, &seg_bytes).expect("BUG: parse seg");
        let total_ticks: u64 = frames.iter().map(|f| u64::from(f.duration)).sum();
        let total_seconds =
            Duration::from_nanos(total_ticks * 1_000_000_000 / u64::from(init.timescale))
                .as_secs_f64();
        assert!(
            total_seconds > 5.0 && total_seconds < 7.0,
            "segment duration off: {total_seconds}s (timescale={})",
            init.timescale
        );
    }
}

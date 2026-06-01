use std::io::{Cursor, Read, Seek, SeekFrom};

use kithara_stream::AudioCodec;
use re_mp4::{BoxHeader, BoxType, FourCC, MoofBox, Mp4, ReadBox, StsdBoxContent};

use crate::error::{DecodeError, DecodeResult};

const FOURCC_FLAC: u32 = 0x664c_6143;

/// Codec-specific decoder config bytes carried in the init segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CodecConfig {
    /// AAC `AudioSpecificConfig` bytes (`ESDS` `DecoderSpecificInfo` body).
    Aac(Vec<u8>),
    /// FLAC `STREAMINFO` block payload (34 bytes, no metadata header).
    Flac(Vec<u8>),
}

impl AsRef<[u8]> for CodecConfig {
    fn as_ref(&self) -> &[u8] {
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
pub(crate) fn parse_init(bytes: &[u8]) -> DecodeResult<Fmp4InitInfo> {
    let mp4 =
        Mp4::read_bytes(bytes).map_err(|e| DecodeError::InvalidData(format!("re_mp4: {e}")))?;

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
            let asc = extract_aac_asc_raw(bytes)?;
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

    let gapless = {
        let mut cursor = Cursor::new(bytes);
        crate::gapless::probe_mp4_gapless(&mut cursor).unwrap_or(None)
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
    const TAG_ES_DESCRIPTOR: u8 = 0x03;
    const TAG_DECODER_CONFIG: u8 = 0x04;
    const TAG_DECODER_SPECIFIC: u8 = 0x05;

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
        .map_err(|e| DecodeError::InvalidData(format!("seek past stsd header: {e}")))?;

    let entry_start = cursor.position();
    let (entry_type, entry_size) = read_header(&mut cursor)?;
    if u32::from(entry_type) != FOURCC_MP4A {
        return Err(DecodeError::InvalidData(format!(
            "expected mp4a sample entry, found {:?}",
            FourCC::from(entry_type).value
        )));
    }
    let entry_end = entry_start + entry_size;
    let _ = stsd_end;

    cursor
        .seek(SeekFrom::Current(28))
        .map_err(|e| DecodeError::InvalidData(format!("seek past mp4a header: {e}")))?;

    while cursor.position() < entry_end {
        let child_start = cursor.position();
        let (child_type, child_size) = read_header(&mut cursor)?;
        if u32::from(child_type) == FOURCC_ESDS {
            cursor
                .seek(SeekFrom::Current(4))
                .map_err(|e| DecodeError::InvalidData(format!("seek past esds header: {e}")))?;
            return read_esds_decoder_specific_info(
                &mut cursor,
                child_start + child_size,
                TAG_ES_DESCRIPTOR,
                TAG_DECODER_CONFIG,
                TAG_DECODER_SPECIFIC,
            );
        }
        cursor
            .seek(SeekFrom::Start(child_start + child_size))
            .map_err(|e| DecodeError::InvalidData(format!("skip mp4a child: {e}")))?;
    }
    Err(DecodeError::InvalidData("esds box not found".into()))
}

/// Walk the descriptor chain `ES_Descriptor` → `DecoderConfigDescriptor`
/// → `DecoderSpecificInfo` inside an `esds` payload and return the
/// DSI body bytes. Each descriptor uses ISO/IEC 14496-1 tag+length
/// framing: a single-byte tag followed by an expandable-size big-endian
/// 7-bit-per-byte length (up to 4 bytes).
fn read_esds_decoder_specific_info(
    cursor: &mut Cursor<&[u8]>,
    esds_end: u64,
    tag_es: u8,
    tag_dec_config: u8,
    tag_dsi: u8,
) -> DecodeResult<Vec<u8>> {
    let (es_tag, es_size) = read_descriptor_header(cursor)?;
    if es_tag != tag_es {
        return Err(DecodeError::InvalidData(format!(
            "expected ES_Descriptor (0x03), got 0x{es_tag:02x}"
        )));
    }
    let es_body_end = cursor.position() + u64::from(es_size);
    let mut header = [0u8; 3];
    cursor
        .read_exact(&mut header)
        .map_err(|e| DecodeError::InvalidData(format!("read ES_Descriptor header: {e}")))?;
    let flags = header[2];
    if flags & 0x80 != 0 {
        cursor
            .seek(SeekFrom::Current(2))
            .map_err(|e| DecodeError::InvalidData(format!("skip dependsOn_ES_ID: {e}")))?;
    }
    if flags & 0x40 != 0 {
        let mut url_len = [0u8; 1];
        cursor
            .read_exact(&mut url_len)
            .map_err(|e| DecodeError::InvalidData(format!("read URL_length: {e}")))?;
        cursor
            .seek(SeekFrom::Current(i64::from(url_len[0])))
            .map_err(|e| DecodeError::InvalidData(format!("skip URL: {e}")))?;
    }
    if flags & 0x20 != 0 {
        cursor
            .seek(SeekFrom::Current(2))
            .map_err(|e| DecodeError::InvalidData(format!("skip OCR_ES_ID: {e}")))?;
    }

    let _ = es_body_end;
    let (dc_tag, dc_size) = read_descriptor_header(cursor)?;
    if dc_tag != tag_dec_config {
        return Err(DecodeError::InvalidData(format!(
            "expected DecoderConfigDescriptor (0x04), got 0x{dc_tag:02x}"
        )));
    }
    let dc_end = cursor.position() + u64::from(dc_size);
    cursor
        .seek(SeekFrom::Current(13))
        .map_err(|e| DecodeError::InvalidData(format!("skip DCD body: {e}")))?;

    let (dsi_tag, dsi_size) = read_descriptor_header(cursor)?;
    if dsi_tag != tag_dsi {
        return Err(DecodeError::InvalidData(format!(
            "expected DecoderSpecificInfo (0x05), got 0x{dsi_tag:02x}"
        )));
    }
    if cursor.position() + u64::from(dsi_size) > dc_end.min(esds_end) {
        return Err(DecodeError::InvalidData(
            "DSI extends past parent descriptor".into(),
        ));
    }
    let mut payload = vec![0u8; dsi_size as usize];
    cursor
        .read_exact(&mut payload)
        .map_err(|e| DecodeError::InvalidData(format!("read DSI body: {e}")))?;
    Ok(payload)
}

/// MPEG-4 descriptor header: 1-byte tag + variable-length size (each
/// size byte's MSB is a continuation flag, low 7 bits feed the running
/// size value). Capped at 4 size bytes per the spec.
fn read_descriptor_header(cursor: &mut Cursor<&[u8]>) -> DecodeResult<(u8, u32)> {
    let mut tag = [0u8; 1];
    cursor
        .read_exact(&mut tag)
        .map_err(|e| DecodeError::InvalidData(format!("read descriptor tag: {e}")))?;
    let mut size: u32 = 0;
    for _ in 0..4 {
        let mut b = [0u8; 1];
        cursor
            .read_exact(&mut b)
            .map_err(|e| DecodeError::InvalidData(format!("read descriptor size byte: {e}")))?;
        size = (size << 7) | u32::from(b[0] & 0x7F);
        if b[0] & 0x80 == 0 {
            return Ok((tag[0], size));
        }
    }
    Err(DecodeError::InvalidData(
        "descriptor size length exceeds 4 bytes".into(),
    ))
}

/// Locate `fLaC` sample entry inside the init bytes and read its
/// associated `dfLa` box payload (FLAC STREAMINFO).
fn parse_flac_sample_entry(bytes: &[u8]) -> DecodeResult<(u32, u16, Vec<u8>)> {
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
        .map_err(|e| DecodeError::InvalidData(format!("seek past stsd header: {e}")))?;

    let entry_start = cursor.position();
    let (entry_type, entry_size) = read_header(&mut cursor)?;
    if u32::from(entry_type) != FOURCC_FLAC {
        return Err(DecodeError::InvalidData(format!(
            "expected fLaC sample entry, found {:?}",
            FourCC::from(entry_type).value
        )));
    }
    let entry_end = entry_start + entry_size;

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

    while cursor.position() < entry_end {
        let inner_start = cursor.position();
        let (inner_type, inner_size) = read_header(&mut cursor)?;
        if u32::from(inner_type) == FOURCC_DFLA {
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

    let sample_total: usize = traf
        .truns
        .iter()
        .map(|trun| usize::try_from(trun.sample_count).unwrap_or(0))
        .sum();
    out.reserve(sample_total);

    for trun in &traf.truns {
        let data_offset_i32 = trun.data_offset.unwrap_or(0);
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
                decode_time,
                duration,
                offset: start,
                size: size as usize,
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
    use kithara_platform::time::Duration;
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
        let init = parse_init(&bytes).expect("BUG: parse init");
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
    fn parse_init_flac_extracts_streaminfo() {
        let bytes = read_fixture("init-slossless-a1.mp4");
        let init = parse_init(&bytes).expect("BUG: parse FLAC init");
        assert_eq!(init.codec, AudioCodec::Flac);
        assert!(matches!(init.config, CodecConfig::Flac(_)));
        let len = init.config.as_ref().len();
        assert_eq!(len, 34, "STREAMINFO body must be 34 bytes");
    }

    #[kithara::test]
    fn parse_segment_frames_aac_yields_monotonic_frames() {
        let init_bytes = read_fixture("init-slq-a1.mp4");
        let init = parse_init(&init_bytes).expect("BUG: parse init");
        let seg_bytes = read_fixture("segment-1-slq-a1.m4s");
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
    fn parse_segment_frames_presizes_vec_to_sample_count() {
        let init_bytes = read_fixture("init-slq-a1.mp4");
        let init = parse_init(&init_bytes).expect("BUG: parse init");
        let seg_bytes = read_fixture("segment-1-slq-a1.m4s");
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
    fn parse_segment_frames_total_duration_matches_extinf() {
        let init_bytes = read_fixture("init-slq-a1.mp4");
        let init = parse_init(&init_bytes).expect("BUG: parse init");
        let seg_bytes = read_fixture("segment-1-slq-a1.m4s");
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

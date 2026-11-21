use std::io::{Cursor, Read, Seek, SeekFrom};

use re_mp4::{BoxHeader, BoxType};

use crate::{DecodeError, DecodeResult, consts};

/// Extract the raw `DecoderSpecificInfo` body from the `mp4a`/`esds` descriptor chain.
/// `re_mp4` retains only profile/rate/channel fields, losing explicit HE-AAC extension bytes;
/// returning the body verbatim preserves the complete configuration for fdk-aac.
pub(super) fn extract_aac_asc_raw(bytes: &[u8]) -> DecodeResult<Vec<u8>> {
    const FOURCC_MP4A: u32 = 0x6d70_3461;
    const FOURCC_ESDS: u32 = 0x6573_6473;

    let (mut cursor, entry_end) =
        open_sample_entry(bytes, FOURCC_MP4A, "expected mp4a sample entry")?;

    cursor
        .seek(SeekFrom::Current(28))
        .map_err(|e| DecodeError::parse("seek past mp4a header", e))?;

    while cursor.position() < entry_end {
        let (child_type, child_end) = read_child_header(&mut cursor, entry_end)?;
        if u32::from(child_type) == FOURCC_ESDS {
            cursor
                .seek(SeekFrom::Current(4))
                .map_err(|e| DecodeError::parse("seek past esds header", e))?;
            return read_esds_decoder_specific_info(&mut cursor, child_end);
        }
        cursor
            .seek(SeekFrom::Start(child_end))
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
    if cursor.position() + u64::from(dsi_size) > dc_end.min(es_body_end).min(esds_end) {
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
        size = (size << 7) | u32::from(b[0] & 0x7f);
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
pub(super) fn parse_flac_sample_entry(
    bytes: &[u8],
) -> DecodeResult<(u32, u16, [u8; consts::FLAC_STREAMINFO_BYTES])> {
    const FOURCC_DFLA: u32 = 0x6466_4c61;

    let (mut cursor, entry_end) =
        open_sample_entry(bytes, consts::FOURCC_FLAC, "expected fLaC sample entry")?;

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
        let (inner_type, inner_end) = read_child_header(&mut cursor, entry_end)?;
        if u32::from(inner_type) == FOURCC_DFLA {
            cursor
                .seek(SeekFrom::Current(4 + 4))
                .map_err(|e| DecodeError::parse("seek past dfLa header", e))?;
            let mut payload = [0u8; consts::FLAC_STREAMINFO_BYTES];
            cursor
                .read_exact(&mut payload)
                .map_err(|e| DecodeError::parse("read STREAMINFO", e))?;
            return Ok((sample_rate, channels, payload));
        }
        cursor
            .seek(SeekFrom::Start(inner_end))
            .map_err(|e| DecodeError::parse("skip sample entry child", e))?;
    }
    Err(DecodeError::InvalidData {
        detail: "dfLa box not found",
    })
}

fn open_sample_entry<'a>(
    bytes: &'a [u8],
    expected: u32,
    detail: &'static str,
) -> DecodeResult<(Cursor<&'a [u8]>, u64)> {
    let mut cursor = Cursor::new(bytes);
    let mut end = bytes.len() as u64;
    for target in [
        BoxType::MoovBox,
        BoxType::TrakBox,
        BoxType::MdiaBox,
        BoxType::MinfBox,
        BoxType::StblBox,
        BoxType::StsdBox,
    ] {
        end = descend_into(&mut cursor, end, target)?;
    }
    cursor
        .seek(SeekFrom::Current(8))
        .map_err(|error| DecodeError::parse("seek past stsd header", error))?;
    let (entry_type, entry_end) = read_child_header(&mut cursor, end)?;
    if u32::from(entry_type) != expected {
        return Err(DecodeError::InvalidData { detail });
    }
    Ok((cursor, entry_end))
}

fn descend_into(cursor: &mut Cursor<&[u8]>, end: u64, target: BoxType) -> DecodeResult<u64> {
    while cursor.position() < end {
        let (box_type, child_end) = read_child_header(cursor, end)?;
        if box_type == target {
            return Ok(child_end);
        }
        cursor
            .seek(SeekFrom::Start(child_end))
            .map_err(|error| DecodeError::parse("skip box", error))?;
    }
    Err(DecodeError::InvalidData {
        detail: "target box not found",
    })
}

fn read_child_header(cursor: &mut Cursor<&[u8]>, parent_end: u64) -> DecodeResult<(BoxType, u64)> {
    let header = BoxHeader::read(cursor).map_err(|error| DecodeError::parse("re_mp4", error))?;
    let end = header
        .size
        .checked_sub(8)
        .and_then(|payload_size| cursor.position().checked_add(payload_size))
        .filter(|end| *end >= cursor.position() && *end <= parent_end)
        .ok_or(DecodeError::InvalidData {
            detail: "sample entry box extends past parent",
        })?;
    Ok((header.name, end))
}

#[cfg(test)]
mod tests {
    #[cfg(not(target_arch = "wasm32"))]
    use kithara_test_fixtures::unit_fixtures::{aac_init, flac_init};
    use kithara_test_utils::kithara;

    use super::*;

    #[cfg(not(target_arch = "wasm32"))]
    fn extended_ancestors(bytes: &[u8], path: &[[u8; 4]]) -> Vec<u8> {
        let mut offset = 0;
        while offset + 8 <= bytes.len() {
            let size = u32::from_be_bytes(
                bytes[offset..offset + 4]
                    .try_into()
                    .expect("fixture box size"),
            ) as usize;
            assert!(size >= 8 && offset + size <= bytes.len());
            if bytes[offset + 4..offset + 8] == path[0] {
                let body = &bytes[offset + 8..offset + size];
                let body = if path.len() == 1 {
                    body.to_vec()
                } else {
                    extended_ancestors(body, &path[1..])
                };
                let mut extended = bytes[..offset].to_vec();
                extended.extend_from_slice(&1u32.to_be_bytes());
                extended.extend_from_slice(&path[0]);
                extended.extend_from_slice(&(body.len() as u64 + 16).to_be_bytes());
                extended.extend_from_slice(&body);
                extended.extend_from_slice(&bytes[offset + size..]);
                return extended;
            }
            offset += size;
        }
        panic!("fixture has no box {:?}", path[0]);
    }

    #[kithara::test(native)]
    fn extended_ancestors_preserve_aac_decoder_configuration(aac_init: Vec<u8>) {
        let expected = extract_aac_asc_raw(&aac_init).expect("AAC fixture configuration");
        let actual =
            extract_aac_asc_raw(&extended_ancestors(&aac_init, &consts::SAMPLE_TABLE_PATH))
                .expect("AAC configuration in extended ancestors");
        assert_eq!(actual, expected);
    }

    #[kithara::test(native)]
    fn extended_ancestors_preserve_flac_decoder_configuration(flac_init: Vec<u8>) {
        let expected = parse_flac_sample_entry(&flac_init).expect("FLAC fixture configuration");
        let actual =
            parse_flac_sample_entry(&extended_ancestors(&flac_init, &consts::SAMPLE_TABLE_PATH))
                .expect("FLAC configuration in extended ancestors");
        assert_eq!(actual, expected);
    }

    #[kithara::test(native, flash(false))]
    fn decoder_specific_info_cannot_extend_past_the_es_descriptor() {
        let bytes = [
            0x03, 0x03, 0, 0, 0, 0x04, 0x11, 0x40, 0x15, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x05,
            0x02, 0x12, 0x10,
        ];
        let mut cursor = Cursor::new(bytes.as_slice());
        let result = read_esds_decoder_specific_info(&mut cursor, bytes.len() as u64);
        assert!(matches!(
            result,
            Err(DecodeError::InvalidData {
                detail: "DSI extends past parent descriptor"
            })
        ));
    }
}

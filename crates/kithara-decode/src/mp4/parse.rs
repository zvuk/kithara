//! Payload parsers for the MP4 boxes the scanner reads.

use smallvec::SmallVec;

use super::{ItunSmpb, Mp4EditListEntry, Mp4MediaTiming, Mp4MetadataError};
use crate::consts;

pub(super) fn invalid(message: impl Into<String>) -> Mp4MetadataError {
    Mp4MetadataError::InvalidData(message.into())
}

/// Extract the four-character codec tag of the first sample entry of an
/// `stsd` (`mp4a`, `fLaC`, `alac`, …). Layout: 4-byte version+flags,
/// 4-byte entry count, then the sample entry (4-byte size, 4-byte format
/// tag). Returns `None` if the payload is too short.
pub(super) fn parse_stsd_codec(payload: &[u8]) -> Option<[u8; 4]> {
    let tag = payload.get(12..16)?;
    Some([tag[0], tag[1], tag[2], tag[3]])
}

/// Extract the audio sample rate from the first sample entry of an `stsd`.
/// Returns `None` if the payload is too short or the entry slot is missing.
pub(super) fn parse_stsd_sample_rate(payload: &[u8]) -> Option<u32> {
    let entry_payload = payload.get(16..)?;
    if entry_payload.len() < 28 {
        return None;
    }
    Some(read_be_u32(&entry_payload[24..28])? >> 16)
}

pub(super) fn parse_elst(
    payload: &[u8],
) -> Result<SmallVec<[Mp4EditListEntry; 1]>, Mp4MetadataError> {
    let version = *payload.first().ok_or_else(|| invalid("elst is empty"))?;
    let entry_count = read_be_u32(slice(payload, 4, 4, "elst entry count")?)
        .ok_or_else(|| invalid("elst entry count is truncated"))? as usize;

    let entry_count = entry_count.min(consts::ELST_MAX_ENTRIES);
    let mut offset = 8;
    let mut entries = SmallVec::with_capacity(entry_count);

    for _ in 0..entry_count {
        let entry = match version {
            0 => parse_elst_v0(payload, &mut offset)?,
            1 => parse_elst_v1(payload, &mut offset)?,
            _ => return Err(invalid(format!("unsupported elst version {version}"))),
        };
        entries.push(entry);
    }

    Ok(entries)
}

pub(super) fn parse_tkhd_track_id(payload: &[u8]) -> Option<u32> {
    let offset = match payload.first()? {
        0 => 12,
        1 => 20,
        _ => return None,
    };
    read_be_u32(payload.get(offset..offset + 4)?)
}

/// Parses an iTunes `data` sub-box. Returns the 24-bit type code (with the
/// version byte stripped) along with the raw value bytes.
pub(super) fn parse_data_box(payload: &[u8]) -> Option<(u32, &[u8])> {
    let header = read_be_u32(payload.get(..4)?)?;
    let value = payload.get(8..)?;
    Some((header & 0x00FF_FFFF, value))
}

/// Parse an iTunSMPB ASCII payload into typed leading/trailing frame counts.
///
/// Layout (whitespace-separated hex tokens): `<version> <leading> <trailing> <total> ...`.
/// We only look at the two padding fields; everything else is ignored.
pub(super) fn parse_itunsmpb(value: &[u8]) -> Option<ItunSmpb> {
    let text = std::str::from_utf8(value).ok()?.trim_end_matches('\0');
    let mut tokens = text.split_ascii_whitespace();
    let _version = tokens.next()?;
    let leading = u64::from_str_radix(tokens.next()?, 16).ok()?;
    let trailing = u64::from_str_radix(tokens.next()?, 16).ok()?;
    Some(ItunSmpb {
        leading_frames: leading,
        trailing_frames: trailing,
    })
}

/// Borrow-style read of a `mean`/`name` text box: skips the 4-byte `FullBox`
/// header and copies the trimmed text into a small inline buffer.
pub(super) fn read_text_fullbox_bytes(payload: &[u8]) -> Option<SmallVec<[u8; 32]>> {
    let body = payload.get(4..)?;
    let trimmed = body
        .iter()
        .rposition(|byte| *byte != 0)
        .map_or(&body[..0], |last| &body[..=last]);
    Some(SmallVec::from_slice(trimmed))
}

pub(super) fn parse_mvhd_timescale(payload: &[u8]) -> Option<u32> {
    let version = *payload.first()?;
    match version {
        0 if payload.len() >= 16 => read_be_u32(&payload[12..16]),
        1 if payload.len() >= 24 => read_be_u32(&payload[20..24]),
        _ => None,
    }
}

pub(super) fn parse_mdhd(payload: &[u8]) -> Option<Mp4MediaTiming> {
    let version = *payload.first()?;
    match version {
        0 if payload.len() >= 20 => {
            let timescale = read_be_u32(&payload[12..16])?;
            let duration = u64::from(read_be_u32(&payload[16..20])?);
            Some(Mp4MediaTiming {
                timescale,
                duration,
            })
        }
        1 if payload.len() >= 32 => {
            let timescale = read_be_u32(&payload[20..24])?;
            let duration = read_be_u64(&payload[24..32])?;
            Some(Mp4MediaTiming {
                timescale,
                duration,
            })
        }
        _ => None,
    }
}

fn slice<'a>(
    data: &'a [u8],
    start: usize,
    len: usize,
    label: &str,
) -> Result<&'a [u8], Mp4MetadataError> {
    data.get(start..start + len)
        .ok_or_else(|| invalid(format!("{label} is truncated")))
}

pub(super) fn read_be_u32(data: &[u8]) -> Option<u32> {
    let bytes: [u8; 4] = data.get(..4)?.try_into().ok()?;
    Some(u32::from_be_bytes(bytes))
}

pub(super) fn read_be_u64(data: &[u8]) -> Option<u64> {
    let bytes: [u8; 8] = data.get(..8)?.try_into().ok()?;
    Some(u64::from_be_bytes(bytes))
}

fn read_be_i32(data: &[u8]) -> Option<i32> {
    let bytes: [u8; 4] = data.get(..4)?.try_into().ok()?;
    Some(i32::from_be_bytes(bytes))
}

fn read_be_i64(data: &[u8]) -> Option<i64> {
    let bytes: [u8; 8] = data.get(..8)?.try_into().ok()?;
    Some(i64::from_be_bytes(bytes))
}
fn parse_elst_v0(payload: &[u8], offset: &mut usize) -> Result<Mp4EditListEntry, Mp4MetadataError> {
    let segment_duration = u64::from(
        read_be_u32(slice(payload, *offset, 4, "elst v0 segment duration")?)
            .ok_or_else(|| invalid("elst v0 segment duration is truncated"))?,
    );
    let media_time = i64::from(
        read_be_i32(slice(payload, *offset + 4, 4, "elst v0 media time")?)
            .ok_or_else(|| invalid("elst v0 media time is truncated"))?,
    );
    let media_rate = read_be_i32(slice(payload, *offset + 8, 4, "elst v0 media rate")?)
        .ok_or_else(|| invalid("elst v0 media rate is truncated"))?;
    *offset += 12;
    Ok(Mp4EditListEntry {
        media_time,
        segment_duration,
        media_rate,
    })
}

fn parse_elst_v1(payload: &[u8], offset: &mut usize) -> Result<Mp4EditListEntry, Mp4MetadataError> {
    let segment_duration = read_be_u64(slice(payload, *offset, 8, "elst v1 segment duration")?)
        .ok_or_else(|| invalid("elst v1 segment duration is truncated"))?;
    let media_time = read_be_i64(slice(payload, *offset + 8, 8, "elst v1 media time")?)
        .ok_or_else(|| invalid("elst v1 media time is truncated"))?;
    let media_rate = read_be_i32(slice(payload, *offset + 16, 4, "elst v1 media rate")?)
        .ok_or_else(|| invalid("elst v1 media rate is truncated"))?;
    *offset += 20;
    Ok(Mp4EditListEntry {
        media_time,
        segment_duration,
        media_rate,
    })
}

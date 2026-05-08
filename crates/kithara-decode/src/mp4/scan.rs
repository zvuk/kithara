use std::{io::SeekFrom, ops::ControlFlow};

use smallvec::SmallVec;
use thiserror::Error;

use crate::traits::DecoderInput;

struct Consts;
impl Consts {
    const BOX_MOOV: [u8; 4] = *b"moov";
    const BOX_MVHD: [u8; 4] = *b"mvhd";
    const BOX_TRAK: [u8; 4] = *b"trak";
    const BOX_META: [u8; 4] = *b"meta";
    const BOX_UDTA: [u8; 4] = *b"udta";
    const BOX_MDIA: [u8; 4] = *b"mdia";
    const BOX_MDHD: [u8; 4] = *b"mdhd";
    const BOX_MINF: [u8; 4] = *b"minf";
    const BOX_STBL: [u8; 4] = *b"stbl";
    const BOX_STSD: [u8; 4] = *b"stsd";
    const BOX_EDTS: [u8; 4] = *b"edts";
    const BOX_ELST: [u8; 4] = *b"elst";
    const BOX_ILST: [u8; 4] = *b"ilst";
    const BOX_FREEFORM: [u8; 4] = *b"----";
    const BOX_DATA: [u8; 4] = *b"data";
    const BOX_MEAN: [u8; 4] = *b"mean";
    const BOX_NAME: [u8; 4] = *b"name";

    const ITUNES_MEAN: &str = "com.apple.iTunes";
    const ITUNSMPB_NAME: &str = "iTunSMPB";

    /// Hard ceiling for `elst` entries to keep adversarial inputs
    /// from forcing huge allocations. Real edit lists in audio files
    /// are tiny.
    const ELST_MAX_ENTRIES: usize = 4096;

    /// Hard ceiling for the `----` payload we are willing to pull
    /// into memory while looking for an iTunSMPB tag. Freeform tags
    /// are kilobytes at most; this stops adversarial inputs from
    /// forcing large allocations during a probe.
    const FREEFORM_MAX_BYTES: usize = 64 * 1024;
}

/// Media-timing pair extracted from an `mdhd` box.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Mp4MediaTiming {
    pub(crate) timescale: u32,
    pub(crate) duration: u64,
}

/// Single `elst` edit-list entry normalized into integer fields.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Mp4EditListEntry {
    pub(crate) segment_duration: u64,
    pub(crate) media_time: i64,
}

/// Typed iTunes "iTunSMPB" payload. The four hex fields are: encoder version
/// (ignored), encoder delay (front padding), encoder padding (trailing
/// silence), and total non-padding sample count. We only surface the two that
/// matter for gapless trimming.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ItunSmpb {
    pub(crate) leading_frames: u64,
    pub(crate) trailing_frames: u64,
}

/// Pull-style visitor invoked while streaming MP4 boxes.
///
/// The scanner only invokes the methods relevant to its current position and
/// honors the returned [`ControlFlow`]: returning `Break(())` from any callback
/// stops the scan as soon as the current box is closed. This lets a consumer
/// build whatever stop condition it needs (e.g. "first track that yields
/// gapless info wins") without the parser knowing anything about the goal.
///
/// All callbacks default to `Continue(())` so consumers only override what
/// they care about.
pub(crate) trait Mp4Visitor {
    fn on_movie_timescale(&mut self, _timescale: u32) -> ControlFlow<()> {
        ControlFlow::Continue(())
    }

    fn on_track_begin(&mut self) -> ControlFlow<()> {
        ControlFlow::Continue(())
    }

    fn on_track_end(&mut self) -> ControlFlow<()> {
        ControlFlow::Continue(())
    }

    fn on_track_sample_rate(&mut self, _sample_rate: u32) -> ControlFlow<()> {
        ControlFlow::Continue(())
    }

    fn on_track_media_timing(&mut self, _timing: Mp4MediaTiming) -> ControlFlow<()> {
        ControlFlow::Continue(())
    }

    fn on_track_edit_list(&mut self, _entries: &[Mp4EditListEntry]) -> ControlFlow<()> {
        ControlFlow::Continue(())
    }

    fn on_itunsmpb(&mut self, _info: ItunSmpb) -> ControlFlow<()> {
        ControlFlow::Continue(())
    }
}

/// MP4 metadata parsing error.
#[derive(Debug, Error)]
pub(crate) enum Mp4MetadataError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Invalid MP4 data: {0}")]
    InvalidData(String),
}

/// Streams MP4 boxes from `reader`'s **current position**, invoking `visitor`
/// callbacks as relevant boxes are encountered. The reader position is always
/// restored before returning, both on success and on error.
///
/// The scanner does not pull large payloads into memory: `mdat` is skipped via
/// forward seeks, standard `ilst` items (including cover art) are walked past
/// without reading their data, and freeform tags are inspected only up to a
/// strict size ceiling.
pub(crate) fn scan_mp4(
    reader: &mut dyn DecoderInput,
    visitor: &mut dyn Mp4Visitor,
) -> Result<(), Mp4MetadataError> {
    let position = reader.stream_position()?;
    let result = Mp4Scanner::new(reader, visitor).scan();
    let restore = reader.seek(SeekFrom::Start(position));

    match (result, restore) {
        (Ok(()), Ok(_)) => Ok(()),
        (Err(error), _) => Err(error),
        (Ok(()), Err(error)) => Err(error.into()),
    }
}

fn invalid(message: impl Into<String>) -> Mp4MetadataError {
    Mp4MetadataError::InvalidData(message.into())
}

/// Header information for one MP4 box, captured from a streaming reader.
#[derive(Clone, Copy)]
struct BoxRef {
    kind: [u8; 4],
    /// Absolute byte position one past the last byte of the box.
    end: u64,
}

struct Mp4Scanner<'a> {
    reader: &'a mut dyn DecoderInput,
    visitor: &'a mut dyn Mp4Visitor,
}

impl<'a> Mp4Scanner<'a> {
    fn new(reader: &'a mut dyn DecoderInput, visitor: &'a mut dyn Mp4Visitor) -> Self {
        Self { reader, visitor }
    }

    fn scan(mut self) -> Result<(), Mp4MetadataError> {
        while let Some(header) = next_box(self.reader, None)? {
            if header.kind == Consts::BOX_MOOV {
                let _ = self.parse_moov(header.end)?;
                return Ok(());
            }
            self.reader.seek(SeekFrom::Start(header.end))?;
        }
        Ok(())
    }

    /// Iterate child boxes until `end`, invoking `visit` for each. The reader
    /// is positioned at the start of the next sibling after each invocation.
    /// If `visit` returns `Break`, the walk stops immediately and propagates
    /// `Break` to the caller.
    fn walk_children<F>(
        &mut self,
        end: u64,
        mut visit: F,
    ) -> Result<ControlFlow<()>, Mp4MetadataError>
    where
        F: FnMut(&mut Self, BoxRef) -> Result<ControlFlow<()>, Mp4MetadataError>,
    {
        while let Some(header) = next_box(self.reader, Some(end))? {
            let flow = visit(self, header)?;
            self.reader.seek(SeekFrom::Start(header.end))?;
            if flow.is_break() {
                return Ok(ControlFlow::Break(()));
            }
        }
        Ok(ControlFlow::Continue(()))
    }

    fn walk_matching_child<F>(
        &mut self,
        end: u64,
        target_kind: [u8; 4],
        mut visit: F,
    ) -> Result<ControlFlow<()>, Mp4MetadataError>
    where
        F: FnMut(&mut Self, BoxRef) -> Result<ControlFlow<()>, Mp4MetadataError>,
    {
        self.walk_children(end, |this, header| {
            if header.kind == target_kind {
                visit(this, header)
            } else {
                Ok(ControlFlow::Continue(()))
            }
        })
    }

    fn walk_payload_child<F>(
        &mut self,
        end: u64,
        target_kind: [u8; 4],
        label: &'static str,
        mut visit: F,
    ) -> Result<ControlFlow<()>, Mp4MetadataError>
    where
        F: FnMut(&mut Self, Vec<u8>) -> Result<ControlFlow<()>, Mp4MetadataError>,
    {
        self.walk_matching_child(end, target_kind, |this, header| {
            let payload = read_payload(this.reader, header.end, label)?;
            visit(this, payload)
        })
    }

    fn read_text_fullbox_child(
        &mut self,
        header: BoxRef,
        label: &'static str,
    ) -> Result<Option<SmallVec<[u8; 32]>>, Mp4MetadataError> {
        let payload = read_payload(self.reader, header.end, label)?;
        Ok(read_text_fullbox_bytes(&payload))
    }

    fn parse_moov(&mut self, end: u64) -> Result<ControlFlow<()>, Mp4MetadataError> {
        self.walk_children(end, |this, header| match header.kind {
            Consts::BOX_MVHD => {
                let payload = read_payload(this.reader, header.end, "mvhd")?;
                if let Some(timescale) = parse_mvhd_timescale(&payload) {
                    return Ok(this.visitor.on_movie_timescale(timescale));
                }
                Ok(ControlFlow::Continue(()))
            }
            Consts::BOX_TRAK => this.parse_trak(header.end),
            Consts::BOX_META => this.parse_meta(header.end),
            Consts::BOX_UDTA => this.parse_udta(header.end),
            _ => Ok(ControlFlow::Continue(())),
        })
    }

    fn parse_trak(&mut self, end: u64) -> Result<ControlFlow<()>, Mp4MetadataError> {
        if self.visitor.on_track_begin().is_break() {
            return Ok(ControlFlow::Break(()));
        }

        let walk = self.walk_children(end, |this, header| match header.kind {
            Consts::BOX_MDIA => this.parse_mdia(header.end),
            Consts::BOX_EDTS => this.parse_edts(header.end),
            _ => Ok(ControlFlow::Continue(())),
        })?;

        let close = self.visitor.on_track_end();
        Ok(if walk.is_break() || close.is_break() {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        })
    }

    fn parse_mdia(&mut self, end: u64) -> Result<ControlFlow<()>, Mp4MetadataError> {
        self.walk_children(end, |this, header| match header.kind {
            Consts::BOX_MDHD => {
                let payload = read_payload(this.reader, header.end, "mdhd")?;
                if let Some(timing) = parse_mdhd(&payload) {
                    return Ok(this.visitor.on_track_media_timing(timing));
                }
                Ok(ControlFlow::Continue(()))
            }
            Consts::BOX_MINF => this.parse_minf(header.end),
            _ => Ok(ControlFlow::Continue(())),
        })
    }

    fn parse_minf(&mut self, end: u64) -> Result<ControlFlow<()>, Mp4MetadataError> {
        self.walk_matching_child(end, Consts::BOX_STBL, |this, header| {
            this.parse_stbl(header.end)
        })
    }

    fn parse_stbl(&mut self, end: u64) -> Result<ControlFlow<()>, Mp4MetadataError> {
        self.walk_payload_child(end, Consts::BOX_STSD, "stsd", |this, payload| {
            if let Some(sample_rate) = parse_stsd_sample_rate(&payload) {
                return Ok(this.visitor.on_track_sample_rate(sample_rate));
            }
            Ok(ControlFlow::Continue(()))
        })
    }

    fn parse_edts(&mut self, end: u64) -> Result<ControlFlow<()>, Mp4MetadataError> {
        self.walk_payload_child(end, Consts::BOX_ELST, "elst", |this, payload| {
            let entries = parse_elst(&payload)?;
            Ok(this.visitor.on_track_edit_list(&entries))
        })
    }

    fn parse_udta(&mut self, end: u64) -> Result<ControlFlow<()>, Mp4MetadataError> {
        self.walk_matching_child(end, Consts::BOX_META, |this, header| {
            this.parse_meta(header.end)
        })
    }

    /// Handles both ISO/IEC 14496-12 `meta` (with a `FullBox` header) and
    /// `QuickTime` `meta` (no `FullBox` header). The two are distinguished by
    /// peeking at the first four bytes: ISO `meta` always starts with
    /// `[version=0, flags=0,0,0]`, while `QuickTime` `meta` starts with the
    /// size of its first sub-box, which is never zero.
    fn parse_meta(&mut self, end: u64) -> Result<ControlFlow<()>, Mp4MetadataError> {
        let payload_start = self.reader.stream_position()?;
        if end.saturating_sub(payload_start) >= 4 {
            let mut probe = [0; 4];
            self.reader.read_exact(&mut probe)?;
            if probe != [0, 0, 0, 0] {
                self.reader.seek(SeekFrom::Start(payload_start))?;
            }
        }

        self.walk_matching_child(end, Consts::BOX_ILST, |this, header| {
            this.parse_ilst(header.end)
        })
    }

    /// Walks `ilst` children but only descends into freeform (`----`) atoms.
    /// Standard items (`covr`, `aART`, `trkn`, …) are skipped without reading
    /// their `data` payloads — that is what keeps cover art out of memory.
    fn parse_ilst(&mut self, end: u64) -> Result<ControlFlow<()>, Mp4MetadataError> {
        self.walk_matching_child(end, Consts::BOX_FREEFORM, |this, header| {
            this.parse_freeform_tag(header.end)
        })
    }

    /// Parses a freeform (`----`) tag, but only yields a value for the
    /// `("com.apple.iTunes","iTunSMPB")` pair. For anything else, the tag is
    /// walked and its bytes discarded — we never allocate the data payload.
    fn parse_freeform_tag(&mut self, end: u64) -> Result<ControlFlow<()>, Mp4MetadataError> {
        let start = self.reader.stream_position()?;
        let payload_len = end.saturating_sub(start);
        let too_big =
            usize::try_from(payload_len).map_or(true, |len| len > Consts::FREEFORM_MAX_BYTES);
        if payload_len == 0 || too_big {
            return Ok(ControlFlow::Continue(()));
        }

        let mut mean: Option<SmallVec<[u8; 32]>> = None;
        let mut name: Option<SmallVec<[u8; 32]>> = None;
        let mut data_range: Option<(u64, u64)> = None;

        let _ = self.walk_children(end, |this, header| {
            match header.kind {
                Consts::BOX_MEAN => mean = this.read_text_fullbox_child(header, "mean")?,
                Consts::BOX_NAME => name = this.read_text_fullbox_child(header, "name")?,
                Consts::BOX_DATA => {
                    let pos = this.reader.stream_position()?;
                    data_range = Some((pos, header.end));
                }
                _ => {}
            }
            Ok(ControlFlow::Continue(()))
        })?;

        let matches = mean
            .as_deref()
            .is_some_and(|m| m == Consts::ITUNES_MEAN.as_bytes())
            && name
                .as_deref()
                .is_some_and(|n| n == Consts::ITUNSMPB_NAME.as_bytes());
        if !matches {
            return Ok(ControlFlow::Continue(()));
        }

        let Some((data_start, data_end)) = data_range else {
            return Ok(ControlFlow::Continue(()));
        };

        self.reader.seek(SeekFrom::Start(data_start))?;
        let payload = read_payload(self.reader, data_end, "iTunSMPB data")?;
        let Some((_data_type, value)) = parse_data_box(&payload) else {
            return Ok(ControlFlow::Continue(()));
        };
        let Some(info) = parse_itunsmpb(value) else {
            return Ok(ControlFlow::Continue(()));
        };

        Ok(self.visitor.on_itunsmpb(info))
    }
}

/// Extract the audio sample rate from the first sample entry of an `stsd`.
/// Returns `None` if the payload is too short or the entry slot is missing.
fn parse_stsd_sample_rate(payload: &[u8]) -> Option<u32> {
    let entry_payload = payload.get(16..)?;
    if entry_payload.len() < 28 {
        return None;
    }
    Some(read_be_u32(&entry_payload[24..28])? >> 16)
}

fn parse_elst(payload: &[u8]) -> Result<SmallVec<[Mp4EditListEntry; 1]>, Mp4MetadataError> {
    let version = *payload.first().ok_or_else(|| invalid("elst is empty"))?;
    let entry_count = read_be_u32(slice(payload, 4, 4, "elst entry count")?)
        .ok_or_else(|| invalid("elst entry count is truncated"))? as usize;

    let entry_count = entry_count.min(Consts::ELST_MAX_ENTRIES);
    let mut offset = 8;
    let mut entries = SmallVec::with_capacity(entry_count);

    for _ in 0..entry_count {
        let entry = match version {
            0 => {
                let segment_duration = u64::from(
                    read_be_u32(slice(payload, offset, 4, "elst v0 segment duration")?)
                        .ok_or_else(|| invalid("elst v0 segment duration is truncated"))?,
                );
                let media_time = i64::from(
                    read_be_i32(slice(payload, offset + 4, 4, "elst v0 media time")?)
                        .ok_or_else(|| invalid("elst v0 media time is truncated"))?,
                );
                offset += 12;
                Mp4EditListEntry {
                    segment_duration,
                    media_time,
                }
            }
            1 => {
                let segment_duration =
                    read_be_u64(slice(payload, offset, 8, "elst v1 segment duration")?)
                        .ok_or_else(|| invalid("elst v1 segment duration is truncated"))?;
                let media_time = read_be_i64(slice(payload, offset + 8, 8, "elst v1 media time")?)
                    .ok_or_else(|| invalid("elst v1 media time is truncated"))?;
                offset += 20;
                Mp4EditListEntry {
                    segment_duration,
                    media_time,
                }
            }
            _ => return Err(invalid(format!("unsupported elst version {version}"))),
        };
        entries.push(entry);
    }

    Ok(entries)
}

/// Parses an iTunes `data` sub-box. Returns the 24-bit type code (with the
/// version byte stripped) along with the raw value bytes.
fn parse_data_box(payload: &[u8]) -> Option<(u32, &[u8])> {
    let header = read_be_u32(payload.get(..4)?)?;
    let value = payload.get(8..)?;
    Some((header & 0x00FF_FFFF, value))
}

/// Parse an iTunSMPB ASCII payload into typed leading/trailing frame counts.
///
/// Layout (whitespace-separated hex tokens): `<version> <leading> <trailing> <total> ...`.
/// We only look at the two padding fields; everything else is ignored.
fn parse_itunsmpb(value: &[u8]) -> Option<ItunSmpb> {
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
fn read_text_fullbox_bytes(payload: &[u8]) -> Option<SmallVec<[u8; 32]>> {
    let body = payload.get(4..)?;
    let trimmed = body
        .iter()
        .rposition(|byte| *byte != 0)
        .map_or(&body[..0], |last| &body[..=last]);
    Some(SmallVec::from_slice(trimmed))
}

fn parse_mvhd_timescale(payload: &[u8]) -> Option<u32> {
    let version = *payload.first()?;
    match version {
        0 if payload.len() >= 16 => read_be_u32(&payload[12..16]),
        1 if payload.len() >= 24 => read_be_u32(&payload[20..24]),
        _ => None,
    }
}

fn parse_mdhd(payload: &[u8]) -> Option<Mp4MediaTiming> {
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

fn next_box(
    reader: &mut dyn DecoderInput,
    end: Option<u64>,
) -> Result<Option<BoxRef>, Mp4MetadataError> {
    let start = reader.stream_position()?;
    if end.is_some_and(|limit| start >= limit) {
        return Ok(None);
    }

    let mut header = [0; 8];
    match reader.read_exact(&mut header) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
            if end.is_some() {
                return Err(invalid("truncated MP4 box header"));
            }
            return Ok(None);
        }
        Err(error) => return Err(error.into()),
    }

    let size32 = read_be_u32(&header[..4]).ok_or_else(|| invalid("truncated MP4 box size"))?;
    let kind = [header[4], header[5], header[6], header[7]];

    let (header_len, total_size) = match size32 {
        1 => {
            let mut extended = [0; 8];
            reader.read_exact(&mut extended).map_err(|error| {
                if error.kind() == std::io::ErrorKind::UnexpectedEof {
                    invalid("truncated extended MP4 box size")
                } else {
                    error.into()
                }
            })?;
            let extended =
                read_be_u64(&extended).ok_or_else(|| invalid("invalid extended MP4 box size"))?;
            (16u64, extended)
        }
        0 => match end {
            Some(limit) => (8u64, limit - start),
            None => return Ok(None),
        },
        _ => (8u64, u64::from(size32)),
    };

    if total_size < header_len {
        return Err(invalid("MP4 box size is smaller than its header"));
    }

    let box_end = start
        .checked_add(total_size)
        .ok_or_else(|| invalid("MP4 box size overflow"))?;

    if end.is_some_and(|limit| box_end > limit) {
        return Err(invalid("MP4 box extends past available bytes"));
    }

    Ok(Some(BoxRef { kind, end: box_end }))
}

fn read_payload(
    reader: &mut dyn DecoderInput,
    end: u64,
    label: &str,
) -> Result<Vec<u8>, Mp4MetadataError> {
    let start = reader.stream_position()?;
    let payload_len = end
        .checked_sub(start)
        .and_then(|payload_len| usize::try_from(payload_len).ok())
        .ok_or_else(|| invalid(format!("{label} payload range underflow")))?;

    let mut payload = vec![0; payload_len];
    reader.read_exact(&mut payload)?;
    Ok(payload)
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

fn read_be_u32(data: &[u8]) -> Option<u32> {
    let bytes: [u8; 4] = data.get(..4)?.try_into().ok()?;
    Some(u32::from_be_bytes(bytes))
}

fn read_be_u64(data: &[u8]) -> Option<u64> {
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

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

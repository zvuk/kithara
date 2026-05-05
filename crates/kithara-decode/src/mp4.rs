use std::{io::SeekFrom, ops::ControlFlow};

use smallvec::SmallVec;
use thiserror::Error;

use crate::traits::DecoderInput;

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

/// Hard ceiling for `elst` entries to keep adversarial inputs from forcing huge
/// allocations. Real edit lists in audio files are tiny.
const ELST_MAX_ENTRIES: usize = 4096;

/// Hard ceiling for the `----` payload we are willing to pull into memory while
/// looking for an iTunSMPB tag. Freeform tags are kilobytes at most; this stops
/// adversarial inputs from forcing large allocations during a probe.
const FREEFORM_MAX_BYTES: usize = 64 * 1024;

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
            if header.kind == BOX_MOOV {
                // Only one `moov` per file in the wild; nothing else upstream
                // is interesting either. The visitor's break flow is honored
                // inside `parse_moov`; either way we stop after `moov`.
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
            BOX_MVHD => {
                let payload = read_payload(this.reader, header.end, "mvhd")?;
                if let Some(timescale) = parse_mvhd_timescale(&payload) {
                    return Ok(this.visitor.on_movie_timescale(timescale));
                }
                Ok(ControlFlow::Continue(()))
            }
            BOX_TRAK => this.parse_trak(header.end),
            BOX_META => this.parse_meta(header.end),
            BOX_UDTA => this.parse_udta(header.end),
            _ => Ok(ControlFlow::Continue(())),
        })
    }

    fn parse_trak(&mut self, end: u64) -> Result<ControlFlow<()>, Mp4MetadataError> {
        if self.visitor.on_track_begin().is_break() {
            return Ok(ControlFlow::Break(()));
        }

        let walk = self.walk_children(end, |this, header| match header.kind {
            BOX_MDIA => this.parse_mdia(header.end),
            BOX_EDTS => this.parse_edts(header.end),
            _ => Ok(ControlFlow::Continue(())),
        })?;

        // Always close the track even if a child callback asked us to break,
        // so the visitor gets a chance to finalize per-track state.
        let close = self.visitor.on_track_end();
        Ok(if walk.is_break() || close.is_break() {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        })
    }

    fn parse_mdia(&mut self, end: u64) -> Result<ControlFlow<()>, Mp4MetadataError> {
        self.walk_children(end, |this, header| match header.kind {
            BOX_MDHD => {
                let payload = read_payload(this.reader, header.end, "mdhd")?;
                if let Some(timing) = parse_mdhd(&payload) {
                    return Ok(this.visitor.on_track_media_timing(timing));
                }
                Ok(ControlFlow::Continue(()))
            }
            BOX_MINF => this.parse_minf(header.end),
            _ => Ok(ControlFlow::Continue(())),
        })
    }

    fn parse_minf(&mut self, end: u64) -> Result<ControlFlow<()>, Mp4MetadataError> {
        self.walk_matching_child(end, BOX_STBL, |this, header| this.parse_stbl(header.end))
    }

    fn parse_stbl(&mut self, end: u64) -> Result<ControlFlow<()>, Mp4MetadataError> {
        self.walk_payload_child(end, BOX_STSD, "stsd", |this, payload| {
            if let Some(sample_rate) = parse_stsd_sample_rate(&payload) {
                return Ok(this.visitor.on_track_sample_rate(sample_rate));
            }
            Ok(ControlFlow::Continue(()))
        })
    }

    fn parse_edts(&mut self, end: u64) -> Result<ControlFlow<()>, Mp4MetadataError> {
        self.walk_payload_child(end, BOX_ELST, "elst", |this, payload| {
            let entries = parse_elst(&payload)?;
            Ok(this.visitor.on_track_edit_list(&entries))
        })
    }

    fn parse_udta(&mut self, end: u64) -> Result<ControlFlow<()>, Mp4MetadataError> {
        self.walk_matching_child(end, BOX_META, |this, header| this.parse_meta(header.end))
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

        self.walk_matching_child(end, BOX_ILST, |this, header| this.parse_ilst(header.end))
    }

    /// Walks `ilst` children but only descends into freeform (`----`) atoms.
    /// Standard items (`covr`, `aART`, `trkn`, …) are skipped without reading
    /// their `data` payloads — that is what keeps cover art out of memory.
    fn parse_ilst(&mut self, end: u64) -> Result<ControlFlow<()>, Mp4MetadataError> {
        self.walk_matching_child(end, BOX_FREEFORM, |this, header| {
            this.parse_freeform_tag(header.end)
        })
    }

    /// Parses a freeform (`----`) tag, but only yields a value for the
    /// `("com.apple.iTunes","iTunSMPB")` pair. For anything else, the tag is
    /// walked and its bytes discarded — we never allocate the data payload.
    fn parse_freeform_tag(&mut self, end: u64) -> Result<ControlFlow<()>, Mp4MetadataError> {
        let start = self.reader.stream_position()?;
        let payload_len = end.saturating_sub(start);
        let too_big = usize::try_from(payload_len).map_or(true, |len| len > FREEFORM_MAX_BYTES);
        if payload_len == 0 || too_big {
            return Ok(ControlFlow::Continue(()));
        }

        let mut mean: Option<SmallVec<[u8; 32]>> = None;
        let mut name: Option<SmallVec<[u8; 32]>> = None;
        let mut data_range: Option<(u64, u64)> = None;

        // We only collect inside the closure (always Continue), so the
        // returned ControlFlow is uninteresting here — we drive the break
        // decision ourselves below, after we know whether this tag matched.
        let _ = self.walk_children(end, |this, header| {
            match header.kind {
                BOX_MEAN => mean = this.read_text_fullbox_child(header, "mean")?,
                BOX_NAME => name = this.read_text_fullbox_child(header, "name")?,
                BOX_DATA => {
                    let pos = this.reader.stream_position()?;
                    data_range = Some((pos, header.end));
                }
                _ => {}
            }
            Ok(ControlFlow::Continue(()))
        })?;

        let matches = mean.as_deref().is_some_and(|m| m == ITUNES_MEAN.as_bytes())
            && name
                .as_deref()
                .is_some_and(|n| n == ITUNSMPB_NAME.as_bytes());
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
    // Layout: 4 bytes version+flags, 4 bytes entry_count, then nested
    // sample-entry boxes. The first sample entry's box header is 8 bytes,
    // so its inner payload starts at offset 16. For an AudioSampleEntry the
    // 16.16 fixed-point sample rate sits at inner offset 24..28
    // (8 bytes SampleEntry tail + 8 bytes reserved + 4 x 2 bytes = 24).
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

    let entry_count = entry_count.min(ELST_MAX_ENTRIES);
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
            // Top-level box with size 0 means "extends to end of file"; nothing
            // useful follows it for our scan purposes.
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
mod tests {
    use std::{
        io::{Cursor, Read, Seek, SeekFrom},
        ops::ControlFlow,
    };

    use kithara_test_utils::kithara;

    use super::{
        ItunSmpb, Mp4EditListEntry, Mp4MediaTiming, Mp4MetadataError, Mp4Visitor, parse_data_box,
        parse_elst, parse_itunsmpb, parse_mdhd, parse_mvhd_timescale, scan_mp4,
    };

    /// Fixture visitor that simply records every callback. Tests assert on the
    /// recorded sequence to verify scanner behavior.
    #[derive(Default, Debug, PartialEq, Eq)]
    struct RecordingVisitor {
        movie_timescale: Option<u32>,
        tracks: Vec<RecordedTrack>,
        itunsmpb: Option<ItunSmpb>,
        in_track: Option<RecordedTrack>,
    }

    #[derive(Default, Debug, PartialEq, Eq, Clone)]
    struct RecordedTrack {
        sample_rate: Option<u32>,
        timing: Option<Mp4MediaTiming>,
        edit_list: Vec<Mp4EditListEntry>,
    }

    impl Mp4Visitor for RecordingVisitor {
        fn on_movie_timescale(&mut self, timescale: u32) -> ControlFlow<()> {
            self.movie_timescale = Some(timescale);
            ControlFlow::Continue(())
        }
        fn on_track_begin(&mut self) -> ControlFlow<()> {
            self.in_track = Some(RecordedTrack::default());
            ControlFlow::Continue(())
        }
        fn on_track_end(&mut self) -> ControlFlow<()> {
            if let Some(track) = self.in_track.take() {
                self.tracks.push(track);
            }
            ControlFlow::Continue(())
        }
        fn on_track_sample_rate(&mut self, sample_rate: u32) -> ControlFlow<()> {
            if let Some(track) = &mut self.in_track {
                track.sample_rate = Some(sample_rate);
            }
            ControlFlow::Continue(())
        }
        fn on_track_media_timing(&mut self, timing: Mp4MediaTiming) -> ControlFlow<()> {
            if let Some(track) = &mut self.in_track {
                track.timing = Some(timing);
            }
            ControlFlow::Continue(())
        }
        fn on_track_edit_list(&mut self, entries: &[Mp4EditListEntry]) -> ControlFlow<()> {
            if let Some(track) = &mut self.in_track {
                track.edit_list = entries.to_vec();
            }
            ControlFlow::Continue(())
        }
        fn on_itunsmpb(&mut self, info: ItunSmpb) -> ControlFlow<()> {
            self.itunsmpb = Some(info);
            ControlFlow::Continue(())
        }
    }

    fn record(reader: &mut dyn super::DecoderInput) -> RecordingVisitor {
        let mut visitor = RecordingVisitor::default();
        scan_mp4(reader, &mut visitor).expect("scan");
        visitor
    }

    fn atom_parts(kind: &[u8; 4], parts: &[&[u8]]) -> Vec<u8> {
        let payload_len: usize = parts.iter().map(|part| part.len()).sum();
        let size = u32::try_from(payload_len + 8).unwrap_or(u32::MAX);
        let mut out = Vec::with_capacity(payload_len + 8);
        out.extend_from_slice(&size.to_be_bytes());
        out.extend_from_slice(kind);
        for part in parts {
            out.extend_from_slice(part);
        }
        out
    }

    fn atom(kind: &[u8; 4], payload: Vec<u8>) -> Vec<u8> {
        atom_parts(kind, &[payload.as_slice()])
    }

    /// Builds a box with an explicit 64-bit (extended) size header.
    fn extended_atom(kind: &[u8; 4], payload: Vec<u8>) -> Vec<u8> {
        let total = (payload.len() + 16) as u64;
        let mut out = Vec::with_capacity(payload.len() + 16);
        out.extend_from_slice(&1u32.to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(&total.to_be_bytes());
        out.extend_from_slice(&payload);
        out
    }

    fn full_box(kind: &[u8; 4], version: u8, body: Vec<u8>) -> Vec<u8> {
        let header = [version, 0, 0, 0];
        atom_parts(kind, &[&header, body.as_slice()])
    }

    fn mvhd_v0(movie_timescale: u32) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&0u32.to_be_bytes()); // creation_time
        body.extend_from_slice(&0u32.to_be_bytes()); // modification_time
        body.extend_from_slice(&movie_timescale.to_be_bytes());
        body.extend_from_slice(&0u32.to_be_bytes()); // duration
        full_box(b"mvhd", 0, body)
    }

    fn mvhd_v1(movie_timescale: u32) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&0u64.to_be_bytes());
        body.extend_from_slice(&0u64.to_be_bytes());
        body.extend_from_slice(&movie_timescale.to_be_bytes());
        body.extend_from_slice(&0u64.to_be_bytes());
        full_box(b"mvhd", 1, body)
    }

    fn mdhd_v0(media_timescale: u32, media_duration: u32) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&0u32.to_be_bytes());
        body.extend_from_slice(&0u32.to_be_bytes());
        body.extend_from_slice(&media_timescale.to_be_bytes());
        body.extend_from_slice(&media_duration.to_be_bytes());
        body.extend_from_slice(&0u32.to_be_bytes()); // language + pre_defined
        full_box(b"mdhd", 0, body)
    }

    fn mdhd_v1(media_timescale: u32, media_duration: u64) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&0u64.to_be_bytes());
        body.extend_from_slice(&0u64.to_be_bytes());
        body.extend_from_slice(&media_timescale.to_be_bytes());
        body.extend_from_slice(&media_duration.to_be_bytes());
        body.extend_from_slice(&0u32.to_be_bytes());
        full_box(b"mdhd", 1, body)
    }

    fn elst_v0_one_entry(segment_duration: u32, media_time: i32) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&1u32.to_be_bytes());
        body.extend_from_slice(&segment_duration.to_be_bytes());
        body.extend_from_slice(&media_time.to_be_bytes());
        body.extend_from_slice(&1u16.to_be_bytes()); // media_rate_integer
        body.extend_from_slice(&0u16.to_be_bytes()); // media_rate_fraction
        full_box(b"elst", 0, body)
    }

    fn elst_v1_one_entry(segment_duration: u64, media_time: i64) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&1u32.to_be_bytes());
        body.extend_from_slice(&segment_duration.to_be_bytes());
        body.extend_from_slice(&media_time.to_be_bytes());
        body.extend_from_slice(&1u16.to_be_bytes());
        body.extend_from_slice(&0u16.to_be_bytes());
        full_box(b"elst", 1, body)
    }

    fn audio_sample_entry(codec: &[u8; 4], sample_rate: u32) -> Vec<u8> {
        let mut sample_entry = vec![0; 6]; // reserved
        sample_entry.extend_from_slice(&1u16.to_be_bytes()); // data_reference_index
        sample_entry.extend_from_slice(&[0; 8]); // reserved
        sample_entry.extend_from_slice(&2u16.to_be_bytes()); // channelcount
        sample_entry.extend_from_slice(&16u16.to_be_bytes()); // samplesize
        sample_entry.extend_from_slice(&0u16.to_be_bytes()); // pre_defined
        sample_entry.extend_from_slice(&0u16.to_be_bytes()); // reserved
        sample_entry.extend_from_slice(&(sample_rate << 16).to_be_bytes());
        atom(codec, sample_entry)
    }

    fn stsd(sample_rate: u32, codec: &[u8; 4]) -> Vec<u8> {
        let entry = audio_sample_entry(codec, sample_rate);
        let mut body = Vec::new();
        body.extend_from_slice(&1u32.to_be_bytes());
        body.extend_from_slice(&entry);
        full_box(b"stsd", 0, body)
    }

    fn data_box(data_type: u32, value: &[u8]) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&data_type.to_be_bytes());
        body.extend_from_slice(&0u32.to_be_bytes()); // locale
        body.extend_from_slice(value);
        atom(b"data", body)
    }

    fn freeform_text_box(kind: &[u8; 4], text: &str) -> Vec<u8> {
        let mut body = vec![0, 0, 0, 1]; // version + flags
        body.extend_from_slice(text.as_bytes());
        atom(kind, body)
    }

    fn freeform_itunsmpb(text: &str) -> Vec<u8> {
        let mut freeform = Vec::new();
        freeform.extend_from_slice(&freeform_text_box(b"mean", "com.apple.iTunes"));
        freeform.extend_from_slice(&freeform_text_box(b"name", "iTunSMPB"));
        freeform.extend_from_slice(&data_box(1, text.as_bytes()));
        atom(b"----", freeform)
    }

    fn make_track_mp4() -> Vec<u8> {
        let mut stbl = Vec::new();
        stbl.extend_from_slice(&stsd(48_000, b"mp4a"));

        let mut minf = Vec::new();
        minf.extend_from_slice(&atom(b"stbl", stbl));

        let mut mdia = Vec::new();
        mdia.extend_from_slice(&mdhd_v0(48_000, 96_000));
        mdia.extend_from_slice(&atom(b"minf", minf));

        let mut edts = Vec::new();
        edts.extend_from_slice(&elst_v0_one_entry(1_916, 2_112));

        let mut trak = Vec::new();
        trak.extend_from_slice(&atom(b"mdia", mdia));
        trak.extend_from_slice(&atom(b"edts", edts));

        let mut moov = Vec::new();
        moov.extend_from_slice(&mvhd_v0(1_000));
        moov.extend_from_slice(&atom(b"trak", trak));

        atom(b"moov", moov)
    }

    fn make_itunsmpb_mp4(text: &str) -> Vec<u8> {
        let ilst = atom(b"ilst", freeform_itunsmpb(text));

        let mut meta_payload = vec![0, 0, 0, 0];
        meta_payload.extend_from_slice(&ilst);

        let udta = atom(b"meta", meta_payload);

        let mut moov = Vec::new();
        moov.extend_from_slice(&mvhd_v0(1_000));
        moov.extend_from_slice(&atom(b"udta", udta));
        atom(b"moov", moov)
    }

    /// `moov` containing a `QuickTime`-style `meta` box (no `FullBox` header)
    /// embedded directly under `moov` rather than under `udta`, with an
    /// iTunSMPB freeform tag inside.
    fn make_quicktime_meta_mp4(text: &str) -> Vec<u8> {
        let ilst = atom(b"ilst", freeform_itunsmpb(text));
        // QuickTime meta has no FullBox prefix.
        let meta = atom(b"meta", ilst);

        let mut moov = Vec::new();
        moov.extend_from_slice(&mvhd_v0(600));
        moov.extend_from_slice(&meta);
        atom(b"moov", moov)
    }

    struct CountingCursor {
        inner: Cursor<Vec<u8>>,
        bytes_read: usize,
    }

    impl CountingCursor {
        fn new(data: Vec<u8>) -> Self {
            Self {
                inner: Cursor::new(data),
                bytes_read: 0,
            }
        }
    }

    impl Read for CountingCursor {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let bytes_read = self.inner.read(buf)?;
            self.bytes_read += bytes_read;
            Ok(bytes_read)
        }
    }

    impl Seek for CountingCursor {
        fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
            self.inner.seek(pos)
        }
    }

    struct NoEndSeekCursor {
        inner: Cursor<Vec<u8>>,
    }

    impl NoEndSeekCursor {
        fn new(data: Vec<u8>) -> Self {
            Self {
                inner: Cursor::new(data),
            }
        }
    }

    impl Read for NoEndSeekCursor {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.inner.read(buf)
        }
    }

    impl Seek for NoEndSeekCursor {
        fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
            match pos {
                SeekFrom::End(_) => Err(std::io::Error::other("seek end forbidden")),
                _ => self.inner.seek(pos),
            }
        }
    }

    #[kithara::test]
    fn emits_track_callbacks_in_order() {
        let mut reader = Cursor::new(make_track_mp4());
        let recorded = record(&mut reader);

        assert_eq!(recorded.movie_timescale, Some(1_000));
        assert_eq!(
            recorded.tracks,
            vec![RecordedTrack {
                sample_rate: Some(48_000),
                timing: Some(Mp4MediaTiming {
                    timescale: 48_000,
                    duration: 96_000,
                }),
                edit_list: vec![Mp4EditListEntry {
                    segment_duration: 1_916,
                    media_time: 2_112,
                }],
            }]
        );
    }

    #[kithara::test]
    fn emits_v1_mvhd_and_mdhd_with_64bit_durations() {
        let mut stbl = Vec::new();
        stbl.extend_from_slice(&stsd(48_000, b"alac"));

        let mut minf = atom(b"stbl", stbl);
        minf = atom(b"minf", minf);

        let mut mdia = Vec::new();
        mdia.extend_from_slice(&mdhd_v1(96_000, 1u64 << 33));
        mdia.extend_from_slice(&minf);

        let trak = atom(b"trak", atom(b"mdia", mdia));

        let mut moov = Vec::new();
        moov.extend_from_slice(&mvhd_v1(48_000));
        moov.extend_from_slice(&trak);

        let mut reader = Cursor::new(atom(b"moov", moov));
        let recorded = record(&mut reader);

        assert_eq!(recorded.movie_timescale, Some(48_000));
        assert_eq!(
            recorded.tracks,
            vec![RecordedTrack {
                sample_rate: Some(48_000),
                timing: Some(Mp4MediaTiming {
                    timescale: 96_000,
                    duration: 1u64 << 33,
                }),
                edit_list: vec![],
            }]
        );
    }

    #[kithara::test]
    fn emits_v1_elst_with_signed_media_time() {
        let mut moov = Vec::new();
        moov.extend_from_slice(&mvhd_v0(1_000));

        let edts = atom(b"edts", elst_v1_one_entry(2_000, -1));
        moov.extend_from_slice(&atom(b"trak", edts));

        let mut reader = Cursor::new(atom(b"moov", moov));
        let recorded = record(&mut reader);

        assert_eq!(
            recorded.tracks[0].edit_list,
            vec![Mp4EditListEntry {
                segment_duration: 2_000,
                media_time: -1,
            }]
        );
    }

    #[kithara::test]
    fn parses_extended_size_box() {
        let mut file = atom(b"ftyp", b"isom".to_vec());
        file.extend_from_slice(&extended_atom(b"mdat", vec![0; 32]));
        file.extend_from_slice(&make_track_mp4());

        let mut reader = Cursor::new(file);
        let recorded = record(&mut reader);

        assert_eq!(recorded.movie_timescale, Some(1_000));
        assert_eq!(recorded.tracks.len(), 1);
    }

    #[kithara::test]
    fn parses_quicktime_style_meta_without_fullbox_header() {
        let mut reader = Cursor::new(make_quicktime_meta_mp4(
            " 00000000 00000840 00000048 0000000000000000",
        ));
        let recorded = record(&mut reader);

        assert_eq!(
            recorded.itunsmpb,
            Some(ItunSmpb {
                leading_frames: 0x840,
                trailing_frames: 0x48,
            })
        );
    }

    #[kithara::test]
    fn emits_typed_itunsmpb_from_freeform_tag() {
        let mut reader = Cursor::new(make_itunsmpb_mp4(
            " 00000000 00000840 00000048 0000000000000000",
        ));
        let recorded = record(&mut reader);

        assert_eq!(
            recorded.itunsmpb,
            Some(ItunSmpb {
                leading_frames: 0x840,
                trailing_frames: 0x48,
            })
        );
    }

    #[kithara::test]
    fn skips_non_itunsmpb_freeform_without_reading_data() {
        // A `----` whose name is not "iTunSMPB" must not call on_itunsmpb,
        // and must not consume the data-box payload (we just sanity-check the
        // visitor here; payload-skip is enforced by the parser's contract).
        let mut freeform = Vec::new();
        freeform.extend_from_slice(&freeform_text_box(b"mean", "com.apple.iTunes"));
        freeform.extend_from_slice(&freeform_text_box(b"name", "OTHER"));
        freeform.extend_from_slice(&data_box(1, b"value"));
        let ilst = atom(b"ilst", atom(b"----", freeform));

        let mut meta_payload = vec![0, 0, 0, 0];
        meta_payload.extend_from_slice(&ilst);

        let mut moov = Vec::new();
        moov.extend_from_slice(&mvhd_v0(1_000));
        moov.extend_from_slice(&atom(b"udta", atom(b"meta", meta_payload)));

        let mut reader = Cursor::new(atom(b"moov", moov));
        let recorded = record(&mut reader);

        assert_eq!(recorded.itunsmpb, None);
    }

    #[kithara::test]
    fn ignores_standard_ilst_items_including_cover_art() {
        // Build an `ilst` containing a fake covr (with a chunky payload) plus
        // a freeform iTunSMPB. Only the freeform tag should reach the visitor.
        let cover_payload = vec![0xFFu8; 1024];
        let cover = atom(b"covr", data_box(13, &cover_payload));
        let title = atom(&[0xa9, b'n', b'a', b'm'], data_box(1, b"Title"));

        let mut ilst = Vec::new();
        ilst.extend_from_slice(&cover);
        ilst.extend_from_slice(&title);
        ilst.extend_from_slice(&freeform_itunsmpb(
            " 00000000 00000840 00000048 0000000000000000",
        ));

        let mut meta_payload = vec![0, 0, 0, 0];
        meta_payload.extend_from_slice(&atom(b"ilst", ilst));

        let mut moov = Vec::new();
        moov.extend_from_slice(&mvhd_v0(1_000));
        moov.extend_from_slice(&atom(b"udta", atom(b"meta", meta_payload)));

        let mut reader = Cursor::new(atom(b"moov", moov));
        let recorded = record(&mut reader);

        assert_eq!(
            recorded.itunsmpb,
            Some(ItunSmpb {
                leading_frames: 0x840,
                trailing_frames: 0x48,
            })
        );
    }

    #[kithara::test]
    fn visitor_break_stops_scan_at_track_end() {
        // Two trak boxes; the visitor signals Break after the first one.
        let trak1 = atom(b"trak", atom(b"mdia", mdhd_v0(48_000, 96_000)));
        let trak2 = atom(b"trak", atom(b"mdia", mdhd_v0(44_100, 132_300)));

        let mut moov = Vec::new();
        moov.extend_from_slice(&mvhd_v0(1_000));
        moov.extend_from_slice(&trak1);
        moov.extend_from_slice(&trak2);
        let bytes = atom(b"moov", moov);

        struct BreakAfterFirstTrack {
            tracks_seen: u32,
        }
        impl Mp4Visitor for BreakAfterFirstTrack {
            fn on_track_end(&mut self) -> ControlFlow<()> {
                self.tracks_seen += 1;
                ControlFlow::Break(())
            }
        }

        let mut visitor = BreakAfterFirstTrack { tracks_seen: 0 };
        let mut reader = Cursor::new(bytes);
        scan_mp4(&mut reader, &mut visitor).expect("scan");
        assert_eq!(visitor.tracks_seen, 1);
    }

    #[kithara::test]
    fn scan_seeks_past_large_boxes_to_find_moov() {
        let mut file = atom(b"ftyp", b"isom".to_vec());
        file.extend_from_slice(&atom(b"mdat", vec![0; (4 * 1024 * 1024) + 128]));
        file.extend_from_slice(&make_track_mp4());

        let mut reader = CountingCursor::new(file);
        let recorded = record(&mut reader);

        assert_eq!(recorded.movie_timescale, Some(1_000));
        assert_eq!(recorded.tracks.len(), 1);
        assert!(reader.bytes_read < 4_096, "unexpected read amplification");
    }

    #[kithara::test]
    fn scan_restores_reader_position() {
        let mut reader = Cursor::new(make_itunsmpb_mp4(
            " 00000000 00000840 00000048 0000000000000000",
        ));
        reader.seek(SeekFrom::Start(3)).expect("seek inside mp4");

        let mut visitor = RecordingVisitor::default();
        scan_mp4(&mut reader, &mut visitor).expect("scan");

        // Position 3 is mid-`ftyp`-style header — the scan must not have moved
        // the cursor for the caller, regardless of what it read internally.
        assert_eq!(reader.stream_position().expect("position"), 3);
    }

    #[kithara::test]
    fn scan_uses_current_reader_position() {
        // Place a junk prefix in front of a real moov, then position the
        // reader past the prefix. The scan must work from that point and not
        // rewind the reader behind the caller's back.
        let mut bytes = b"junkjunk".to_vec();
        bytes.extend_from_slice(&make_track_mp4());

        let mut reader = Cursor::new(bytes);
        reader.seek(SeekFrom::Start(8)).expect("skip prefix");

        let recorded = record(&mut reader);
        assert_eq!(recorded.movie_timescale, Some(1_000));
        assert_eq!(recorded.tracks.len(), 1);
    }

    #[kithara::test]
    fn scan_does_not_seek_to_end() {
        let mut reader = NoEndSeekCursor::new(make_track_mp4());
        let recorded = record(&mut reader);

        assert_eq!(recorded.movie_timescale, Some(1_000));
        assert_eq!(recorded.tracks.len(), 1);
    }

    #[kithara::test]
    fn top_level_size_zero_terminates_scan_without_error() {
        let mut file = atom(b"ftyp", b"isom".to_vec());
        // Header that says "size = 0", meaning extends to end of file. With
        // moov before this marker, scanning should still succeed.
        file.extend_from_slice(&make_track_mp4());
        file.extend_from_slice(&[0, 0, 0, 0, b'm', b'd', b'a', b't']);

        let mut reader = Cursor::new(file);
        let recorded = record(&mut reader);
        assert_eq!(recorded.tracks.len(), 1);
    }

    #[kithara::test]
    fn rejects_child_box_extending_past_parent() {
        let mut file = atom(b"ftyp", b"isom".to_vec());
        // moov size 24 (8 header + 16 payload). Inside, declare an mvhd of
        // size 128 — well past the parent's end. The scanner must reject this.
        let mut moov_payload = Vec::new();
        moov_payload.extend_from_slice(&128u32.to_be_bytes());
        moov_payload.extend_from_slice(b"mvhd");
        moov_payload.extend_from_slice(&[0; 8]);
        file.extend_from_slice(&atom(b"moov", moov_payload));

        let mut reader = Cursor::new(file);
        let mut visitor = RecordingVisitor::default();
        let error = scan_mp4(&mut reader, &mut visitor).expect_err("must fail");
        assert!(matches!(error, Mp4MetadataError::InvalidData(_)));
    }

    #[kithara::test]
    fn rejects_box_smaller_than_header() {
        let mut file = atom(b"ftyp", b"isom".to_vec());
        file.extend_from_slice(&4u32.to_be_bytes()); // size=4, smaller than 8-byte header
        file.extend_from_slice(b"junk");

        let mut reader = Cursor::new(file);
        let mut visitor = RecordingVisitor::default();
        let error = scan_mp4(&mut reader, &mut visitor).expect_err("must fail");
        assert!(matches!(error, Mp4MetadataError::InvalidData(_)));
    }

    #[kithara::test]
    fn parse_mvhd_timescale_handles_versions_and_truncation() {
        let mut v0 = vec![0, 0, 0, 0]; // version + flags
        v0.extend_from_slice(&[0; 8]); // creation + modification
        v0.extend_from_slice(&44_100u32.to_be_bytes());
        assert_eq!(parse_mvhd_timescale(&v0), Some(44_100));

        let mut v1 = vec![1, 0, 0, 0];
        v1.extend_from_slice(&[0; 16]);
        v1.extend_from_slice(&96_000u32.to_be_bytes());
        assert_eq!(parse_mvhd_timescale(&v1), Some(96_000));

        assert_eq!(parse_mvhd_timescale(&[]), None);
        assert_eq!(parse_mvhd_timescale(&[0, 0, 0, 0]), None);
        assert_eq!(parse_mvhd_timescale(&[2, 0, 0, 0]), None);
    }

    #[kithara::test]
    fn parse_mdhd_handles_versions_and_truncation() {
        let mut v0 = vec![0, 0, 0, 0];
        v0.extend_from_slice(&[0; 8]);
        v0.extend_from_slice(&48_000u32.to_be_bytes());
        v0.extend_from_slice(&100u32.to_be_bytes());
        assert_eq!(
            parse_mdhd(&v0),
            Some(Mp4MediaTiming {
                timescale: 48_000,
                duration: 100,
            })
        );

        let mut v1 = vec![1, 0, 0, 0];
        v1.extend_from_slice(&[0; 16]);
        v1.extend_from_slice(&44_100u32.to_be_bytes());
        v1.extend_from_slice(&(u64::MAX - 1).to_be_bytes());
        assert_eq!(
            parse_mdhd(&v1),
            Some(Mp4MediaTiming {
                timescale: 44_100,
                duration: u64::MAX - 1,
            })
        );

        assert_eq!(parse_mdhd(&[]), None);
        assert_eq!(parse_mdhd(&[0, 0, 0, 0, 1, 2]), None);
    }

    #[kithara::test]
    fn parse_data_box_strips_version_byte_from_type_code() {
        let mut payload = vec![0x01, 0x00, 0x00, 0x0d]; // version=1, type=13 (JPEG)
        payload.extend_from_slice(&0u32.to_be_bytes()); // locale
        payload.extend_from_slice(b"\xff\xd8\xff");

        let (data_type, value) = parse_data_box(&payload).expect("data box");
        assert_eq!(data_type, 13);
        assert_eq!(value, b"\xff\xd8\xff");
    }

    #[kithara::test]
    fn parse_data_box_returns_none_for_short_payload() {
        assert!(parse_data_box(&[]).is_none());
        assert!(parse_data_box(&[0; 3]).is_none());
        assert!(parse_data_box(&[0; 7]).is_none());
    }

    #[kithara::test]
    fn rejects_unsupported_elst_version() {
        let mut payload = vec![3, 0, 0, 0]; // version 3
        payload.extend_from_slice(&1u32.to_be_bytes());
        payload.extend_from_slice(&[0; 12]);

        let error = parse_elst(&payload).expect_err("must fail");
        assert!(matches!(error, Mp4MetadataError::InvalidData(_)));
    }

    #[kithara::test]
    fn elst_caps_entry_count() {
        let mut payload = vec![0, 0, 0, 0];
        payload.extend_from_slice(&u32::MAX.to_be_bytes());

        let error = parse_elst(&payload).expect_err("must fail");
        assert!(matches!(error, Mp4MetadataError::InvalidData(_)));
    }

    #[kithara::test]
    fn parse_itunsmpb_extracts_leading_and_trailing_frames() {
        let info = parse_itunsmpb(b" 00000000 00000840 00000048 0000000000000000")
            .expect("itunsmpb parses");
        assert_eq!(
            info,
            ItunSmpb {
                leading_frames: 0x840,
                trailing_frames: 0x48,
            }
        );
    }

    #[kithara::test]
    fn parse_itunsmpb_rejects_short_or_malformed_input() {
        assert_eq!(parse_itunsmpb(b""), None);
        assert_eq!(parse_itunsmpb(b"only one"), None);
        assert_eq!(parse_itunsmpb(b" 00000000 ZZZZZZZZ 00000048 0"), None);
    }
}

use std::{io::SeekFrom, ops::ControlFlow};

use kithara_bufpool::{HasPool, PoolError, PoolRegion};
use thiserror::Error;

use super::{
    Mp4Event,
    boxes::{BoxRef, next_box, read_payload},
    freeform::read_itunsmpb,
    parse::{
        parse_elst, parse_mdhd, parse_mvhd_timescale, parse_stsd_codec, parse_stsd_sample_rate,
        parse_tkhd_track_id,
    },
};
use crate::{DecoderInput, consts};

/// MP4 metadata parsing error.
#[derive(Debug, Error)]
pub(crate) enum Mp4MetadataError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("buffer allocation failed: {0}")]
    Pool(#[from] PoolError),

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
pub(crate) fn scan_mp4<S>(
    reader: &mut dyn DecoderInput,
    visitor: &mut dyn FnMut(Mp4Event<'_>) -> ControlFlow<()>,
    pools: &PoolRegion<S>,
) -> Result<(), Mp4MetadataError>
where
    S: HasPool<u8>,
{
    let position = reader.stream_position()?;
    let result = Mp4Scanner::new(reader, visitor, pools).scan();
    let restore = reader.seek(SeekFrom::Start(position));

    match (result, restore) {
        (Ok(()), Ok(_)) => Ok(()),
        (Err(error), _) => Err(error),
        (Ok(()), Err(error)) => Err(error.into()),
    }
}

struct Mp4Scanner<'a, S> {
    pools: &'a PoolRegion<S>,
    reader: &'a mut dyn DecoderInput,
    visitor: &'a mut dyn FnMut(Mp4Event<'_>) -> ControlFlow<()>,
}

impl<'a, S> Mp4Scanner<'a, S>
where
    S: HasPool<u8>,
{
    fn new(
        reader: &'a mut dyn DecoderInput,
        visitor: &'a mut dyn FnMut(Mp4Event<'_>) -> ControlFlow<()>,
        pools: &'a PoolRegion<S>,
    ) -> Self {
        Self {
            pools,
            reader,
            visitor,
        }
    }

    fn parse_edts(&mut self, end: u64) -> Result<ControlFlow<()>, Mp4MetadataError> {
        self.walk_payload_child(end, consts::BOX_ELST, "elst", |this, payload| {
            let entries = parse_elst(payload)?;
            Ok((this.visitor)(Mp4Event::TrackEditList(&entries)))
        })
    }

    /// Walks `ilst` children but only descends into freeform (`----`) atoms.
    /// Standard items (`covr`, `aART`, `trkn`, …) are skipped without reading
    /// their `data` payloads — that is what keeps cover art out of memory.
    fn parse_ilst(&mut self, end: u64) -> Result<ControlFlow<()>, Mp4MetadataError> {
        self.walk_matching_child(end, consts::BOX_FREEFORM, |this, header| {
            Ok(match read_itunsmpb(this.reader, header.end, this.pools)? {
                Some(info) => (this.visitor)(Mp4Event::ItunSmpb(info)),
                None => ControlFlow::Continue(()),
            })
        })
    }

    fn parse_mdia(&mut self, end: u64) -> Result<ControlFlow<()>, Mp4MetadataError> {
        self.walk_children(end, |this, header| match header.kind {
            consts::BOX_MDHD => {
                let payload = read_payload(this.reader, header.end, "mdhd", this.pools)?;
                if let Some(timing) = parse_mdhd(&payload) {
                    return Ok((this.visitor)(Mp4Event::TrackMediaTiming(timing)));
                }
                Ok(ControlFlow::Continue(()))
            }
            consts::BOX_MINF => this.parse_minf(header.end),
            _ => Ok(ControlFlow::Continue(())),
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

        self.walk_matching_child(end, consts::BOX_ILST, |this, header| {
            this.parse_ilst(header.end)
        })
    }

    fn parse_minf(&mut self, end: u64) -> Result<ControlFlow<()>, Mp4MetadataError> {
        self.walk_matching_child(end, consts::BOX_STBL, |this, header| {
            this.parse_stbl(header.end)
        })
    }

    fn parse_moov(&mut self, end: u64) -> Result<ControlFlow<()>, Mp4MetadataError> {
        self.walk_children(end, |this, header| match header.kind {
            consts::BOX_MVHD => {
                let payload = read_payload(this.reader, header.end, "mvhd", this.pools)?;
                if let Some(timescale) = parse_mvhd_timescale(&payload) {
                    return Ok((this.visitor)(Mp4Event::MovieTimescale(timescale)));
                }
                Ok(ControlFlow::Continue(()))
            }
            consts::BOX_TRAK => this.parse_trak(header.end),
            consts::BOX_META => this.parse_meta(header.end),
            consts::BOX_UDTA => this.parse_udta(header.end),
            _ => Ok(ControlFlow::Continue(())),
        })
    }

    fn parse_stbl(&mut self, end: u64) -> Result<ControlFlow<()>, Mp4MetadataError> {
        self.walk_payload_child(end, consts::BOX_STSD, "stsd", |this, payload| {
            if let Some(fourcc) = parse_stsd_codec(payload)
                && (this.visitor)(Mp4Event::TrackCodec(fourcc)).is_break()
            {
                return Ok(ControlFlow::Break(()));
            }
            if let Some(sample_rate) = parse_stsd_sample_rate(payload) {
                return Ok((this.visitor)(Mp4Event::TrackSampleRate(sample_rate)));
            }
            Ok(ControlFlow::Continue(()))
        })
    }

    fn parse_trak(&mut self, end: u64) -> Result<ControlFlow<()>, Mp4MetadataError> {
        if (self.visitor)(Mp4Event::TrackBegin).is_break() {
            return Ok(ControlFlow::Break(()));
        }

        let walk = self.walk_children(end, |this, header| match header.kind {
            consts::BOX_TKHD => {
                let payload = read_payload(this.reader, header.end, "tkhd", this.pools)?;
                Ok(
                    parse_tkhd_track_id(&payload).map_or(ControlFlow::Continue(()), |id| {
                        (this.visitor)(Mp4Event::TrackId(id))
                    }),
                )
            }
            consts::BOX_MDIA => this.parse_mdia(header.end),
            consts::BOX_EDTS => this.parse_edts(header.end),
            _ => Ok(ControlFlow::Continue(())),
        })?;

        let close = (self.visitor)(Mp4Event::TrackEnd);
        Ok(if walk.is_break() || close.is_break() {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        })
    }

    fn parse_udta(&mut self, end: u64) -> Result<ControlFlow<()>, Mp4MetadataError> {
        self.walk_matching_child(end, consts::BOX_META, |this, header| {
            this.parse_meta(header.end)
        })
    }
}

impl<S> Mp4Scanner<'_, S>
where
    S: HasPool<u8>,
{
    fn scan(mut self) -> Result<(), Mp4MetadataError> {
        while let Some(header) = next_box(self.reader, None)? {
            if header.kind == consts::BOX_MOOV {
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
        F: FnMut(&mut Self, &[u8]) -> Result<ControlFlow<()>, Mp4MetadataError>,
    {
        self.walk_matching_child(end, target_kind, |this, header| {
            let payload = read_payload(this.reader, header.end, label, this.pools)?;
            visit(this, &payload)
        })
    }
}

/// Sniff the first audio sample-entry codec tag from an MP4 container.
/// Used by the probe path to disambiguate codecs that share the
/// `.m4a`/`.mp4` extension (AAC vs ALAC vs FLAC). Reader position is
/// restored; returns `None` when the container has no parseable `stsd`.
pub(crate) fn sniff_mp4_codec<S>(
    reader: &mut dyn DecoderInput,
    pools: &PoolRegion<S>,
) -> crate::DecodeResult<Option<[u8; 4]>>
where
    S: HasPool<u8>,
{
    let mut fourcc = None;
    let result = scan_mp4(
        reader,
        &mut |event| {
            if let Mp4Event::TrackCodec(codec) = event {
                fourcc = Some(codec);
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        },
        pools,
    );
    match result {
        Ok(()) => Ok(fourcc),
        Err(Mp4MetadataError::Io(error)) => Err(error.into()),
        Err(Mp4MetadataError::Pool(error)) => Err(error.into()),
        Err(Mp4MetadataError::InvalidData(_)) => Ok(None),
    }
}

/// Return whether an MP4 stream is fragmented. Reader position is restored.
pub(crate) fn sniff_mp4_fragmented(reader: &mut dyn DecoderInput) -> Option<bool> {
    let position = reader.stream_position().ok()?;
    let result = scan_mp4_fragment_hint(reader);
    let restore = reader.seek(SeekFrom::Start(position));

    match (result, restore) {
        (Ok(fragmented), Ok(_)) => Some(fragmented),
        _ => None,
    }
}

fn scan_mp4_fragment_hint(reader: &mut dyn DecoderInput) -> Result<bool, Mp4MetadataError> {
    while let Some(header) = next_box(reader, None)? {
        match header.kind {
            consts::BOX_MOOF => return Ok(true),
            consts::BOX_MOOV if scan_moov_for_mvex(reader, header.end)? => return Ok(true),
            _ => {}
        }
        reader.seek(SeekFrom::Start(header.end))?;
    }
    Ok(false)
}

fn scan_moov_for_mvex(reader: &mut dyn DecoderInput, end: u64) -> Result<bool, Mp4MetadataError> {
    while let Some(header) = next_box(reader, Some(end))? {
        if header.kind == consts::BOX_MVEX {
            return Ok(true);
        }
        reader.seek(SeekFrom::Start(header.end))?;
    }
    Ok(false)
}

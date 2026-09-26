use std::{
    num::{NonZeroU32, NonZeroU64},
    ops::Range,
    str,
};

use bytes::{BufMut, Bytes, BytesMut};
use kithara_platform::time::Duration;
use kithara_signal::FrameCoverage;

use crate::{
    AnalysisFingerprint, AnalysisProgress, TrackAnalysis,
    archive::{AnalysisFileError, AnalysisFilePatch, AnalysisFileUpdate, AnalysisFileWrite},
    blob::{MAX_PREALLOC, Reader},
    consts,
};

/// Immutable identity and fixed-chunk layout of one analysis file.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct AnalysisFileSpec {
    fingerprint: AnalysisFingerprint,
    source_sample_rate: NonZeroU32,
    chunk_frames: NonZeroU64,
    extent: u64,
}

impl AnalysisFileSpec {
    /// Define the exact axis, extent, chunk size, and analyzer fingerprints.
    ///
    /// # Errors
    ///
    /// Returns a typed size/configuration error when the fixed header or index
    /// cannot represent the supplied values.
    pub fn new(
        source_sample_rate: NonZeroU32,
        extent: u64,
        chunk_frames: NonZeroU64,
        fingerprint: AnalysisFingerprint,
    ) -> Result<Self, AnalysisFileError> {
        validate_fingerprint(&fingerprint)?;
        let spec = Self {
            fingerprint,
            source_sample_rate,
            chunk_frames,
            extent,
        };
        spec.layout()?;
        Ok(spec)
    }

    /// Fixed analysis chunk size in source frames.
    #[must_use]
    pub const fn chunk_frames(&self) -> NonZeroU64 {
        self.chunk_frames
    }

    /// Stable source extent used to size the fixed index.
    #[must_use]
    pub const fn extent(&self) -> u64 {
        self.extent
    }

    /// Exact active analyzer fingerprints.
    #[must_use]
    pub const fn fingerprint(&self) -> &AnalysisFingerprint {
        &self.fingerprint
    }

    /// Derive a file identity from a snapshot once its extent is known.
    ///
    /// # Errors
    ///
    /// Returns [`AnalysisFileError::UnknownExtent`] for a progressive snapshot
    /// whose fixed index cannot yet be sized.
    pub fn for_analysis(
        analysis: &TrackAnalysis,
        chunk_frames: NonZeroU64,
    ) -> Result<Self, AnalysisFileError> {
        let extent = analysis.extent().ok_or(AnalysisFileError::UnknownExtent)?;
        Self::new(
            analysis.source_sample_rate(),
            extent,
            chunk_frames,
            analysis.fingerprint().clone(),
        )
    }

    fn layout(&self) -> Result<Layout, AnalysisFileError> {
        let chunk_count = self.extent.div_ceil(self.chunk_frames.get());
        let count = usize::try_from(chunk_count).map_err(|_| AnalysisFileError::TooLarge)?;
        if count > MAX_PREALLOC {
            return Err(AnalysisFileError::TooLarge);
        }
        let index_bytes = count
            .checked_mul(consts::INDEX_ENTRY_LEN)
            .ok_or(AnalysisFileError::TooLarge)?;
        let payload_offset = consts::HEADER_LEN
            .checked_add(index_bytes)
            .ok_or(AnalysisFileError::TooLarge)?;
        Ok(Layout {
            chunk_count,
            index_offset: u64::try_from(consts::HEADER_LEN)
                .map_err(|_| AnalysisFileError::TooLarge)?,
            payload_offset: u64::try_from(payload_offset)
                .map_err(|_| AnalysisFileError::TooLarge)?,
        })
    }

    /// Whether the stored frame count exactly matches a configured wall-clock
    /// chunk duration on the stored source-rate axis.
    #[must_use]
    pub fn matches_chunk_duration(&self, duration: Duration) -> bool {
        u128::from(self.chunk_frames.get()) * 1_000_000_000
            == u128::from(self.source_sample_rate.get()) * duration.as_nanos()
    }

    /// Source-frame axis of every payload in the file.
    #[must_use]
    pub const fn source_sample_rate(&self) -> NonZeroU32 {
        self.source_sample_rate
    }
}

/// Parsed indexed analysis file and its latest complete snapshot payload.
#[derive(Debug)]
#[non_exhaustive]
pub struct AnalysisFile {
    spec: AnalysisFileSpec,
    latest: AnalysisProgress,
    header: Header,
    index: Vec<IndexEntry>,
}

impl AnalysisFile {
    /// Build the streaming update that creates a new indexed file.
    ///
    /// # Errors
    ///
    /// Returns a typed configuration, size, or payload encoding error.
    pub fn create(
        spec: &AnalysisFileSpec,
        progress: &AnalysisProgress,
    ) -> Result<AnalysisFileUpdate, AnalysisFileError> {
        build_update(spec, None, progress)
    }

    /// Latest full analysis snapshot stored in the file.
    #[must_use]
    pub const fn latest(&self) -> &AnalysisProgress {
        &self.latest
    }

    /// Parse a committed file and derive its source axis and fixed layout from
    /// the validated header.
    ///
    /// # Errors
    ///
    /// Rejects stale analyzer fingerprints, invalid bounds, overflow,
    /// malformed index entries, and a latest payload inconsistent with the
    /// header/index.
    pub fn parse(
        bytes: &[u8],
        expected_fingerprint: &AnalysisFingerprint,
    ) -> Result<Self, AnalysisFileError> {
        let header_bytes = bytes
            .get(..consts::HEADER_LEN)
            .ok_or(AnalysisFileError::Corrupt)?;
        let header = Header::decode(header_bytes)?;
        if &header.fingerprint != expected_fingerprint {
            return Err(AnalysisFileError::Config);
        }
        let spec = AnalysisFileSpec::new(
            header.source_sample_rate,
            header.extent,
            header.chunk_frames,
            header.fingerprint.clone(),
        )?;
        let layout = spec.layout()?;
        header.validate(layout, bytes.len())?;

        let index_start =
            usize::try_from(header.index_offset).map_err(|_| AnalysisFileError::Corrupt)?;
        let index_end =
            usize::try_from(header.payload_offset).map_err(|_| AnalysisFileError::Corrupt)?;
        let index_bytes = bytes
            .get(index_start..index_end)
            .ok_or(AnalysisFileError::Corrupt)?;
        let count = usize::try_from(header.chunk_count).map_err(|_| AnalysisFileError::Corrupt)?;
        if index_bytes.len() != count.saturating_mul(consts::INDEX_ENTRY_LEN) {
            return Err(AnalysisFileError::Corrupt);
        }

        let mut index: Vec<IndexEntry> = Vec::with_capacity(count);
        for raw in index_bytes.chunks_exact(consts::INDEX_ENTRY_LEN) {
            let entry = IndexEntry::decode(raw)?;
            index.push(entry);
        }

        let latest_start = usize::try_from(header.latest_payload_offset)
            .map_err(|_| AnalysisFileError::Corrupt)?;
        let latest_end = usize::try_from(
            header
                .latest_payload_offset
                .checked_add(header.latest_payload_len)
                .ok_or(AnalysisFileError::Corrupt)?,
        )
        .map_err(|_| AnalysisFileError::Corrupt)?;
        let payload = bytes
            .get(latest_start..latest_end)
            .ok_or(AnalysisFileError::Corrupt)?;
        let latest = AnalysisProgress::try_from((payload, spec.fingerprint()))?;
        let analysis = latest.analysis();
        if validate_analysis(&spec, analysis).is_err() {
            return Err(AnalysisFileError::Corrupt);
        }
        if analysis.revision() != header.latest_revision || analysis.is_settled() != header.settled
        {
            return Err(AnalysisFileError::Corrupt);
        }
        if latest
            .resume_meta()
            .is_some_and(|(chunk_frames, _)| chunk_frames != spec.chunk_frames)
        {
            return Err(AnalysisFileError::Corrupt);
        }

        validate_index_coverage(&spec, &index, analysis)?;
        Ok(Self {
            spec,
            latest,
            header,
            index,
        })
    }

    /// Immutable file identity used during parse and update.
    #[must_use]
    pub const fn spec(&self) -> &AnalysisFileSpec {
        &self.spec
    }

    /// Build a replacement generation with a newer complete snapshot payload.
    /// Already-filled chunk entries remain unchanged; the single payload slot
    /// is replaced and truncated to its new exact length.
    ///
    /// # Errors
    ///
    /// Rejects stale revisions, coverage regression, or configuration drift.
    pub fn update(
        &self,
        progress: &AnalysisProgress,
    ) -> Result<AnalysisFileUpdate, AnalysisFileError> {
        build_update(&self.spec, Some(self), progress)
    }
}

impl From<AnalysisFile> for AnalysisProgress {
    fn from(file: AnalysisFile) -> Self {
        file.latest
    }
}

#[derive(Clone, Copy)]
struct Layout {
    chunk_count: u64,
    index_offset: u64,
    payload_offset: u64,
}

#[derive(Clone, Debug)]
struct Header {
    fingerprint: AnalysisFingerprint,
    source_sample_rate: NonZeroU32,
    chunk_frames: NonZeroU64,
    settled: bool,
    chunk_count: u64,
    extent: u64,
    index_offset: u64,
    latest_payload_len: u64,
    latest_payload_offset: u64,
    latest_revision: u64,
    payload_end: u64,
    payload_offset: u64,
}

impl Header {
    const VERSION: u32 = 0x4b41_4603;

    fn base(spec: &AnalysisFileSpec, layout: Layout) -> Self {
        Self {
            settled: false,
            latest_revision: 0,
            source_sample_rate: spec.source_sample_rate,
            extent: spec.extent,
            chunk_frames: spec.chunk_frames,
            chunk_count: layout.chunk_count,
            index_offset: layout.index_offset,
            payload_offset: layout.payload_offset,
            payload_end: layout.payload_offset,
            latest_payload_offset: layout.payload_offset,
            latest_payload_len: 0,
            fingerprint: spec.fingerprint.clone(),
        }
    }

    fn decode(bytes: &[u8]) -> Result<Self, AnalysisFileError> {
        let mut reader = Reader::new(bytes);
        let version = reader.read_u32().map_err(|_| AnalysisFileError::Corrupt)?;
        if version != Self::VERSION {
            return Err(AnalysisFileError::Version {
                found: version,
                expected: Self::VERSION,
            });
        }
        let settled = reader.read_bool().map_err(|_| AnalysisFileError::Corrupt)?;
        if reader
            .read_array::<3>()
            .map_err(|_| AnalysisFileError::Corrupt)?
            != [0; 3]
        {
            return Err(AnalysisFileError::Corrupt);
        }
        let latest_revision = reader.read_u64().map_err(|_| AnalysisFileError::Corrupt)?;
        let source_sample_rate =
            NonZeroU32::new(reader.read_u32().map_err(|_| AnalysisFileError::Corrupt)?)
                .ok_or(AnalysisFileError::Corrupt)?;
        if reader
            .read_array::<4>()
            .map_err(|_| AnalysisFileError::Corrupt)?
            != [0; 4]
        {
            return Err(AnalysisFileError::Corrupt);
        }
        let extent = reader.read_u64().map_err(|_| AnalysisFileError::Corrupt)?;
        let chunk_frames =
            NonZeroU64::new(reader.read_u64().map_err(|_| AnalysisFileError::Corrupt)?)
                .ok_or(AnalysisFileError::Corrupt)?;
        let chunk_count = reader.read_u64().map_err(|_| AnalysisFileError::Corrupt)?;
        let index_offset = reader.read_u64().map_err(|_| AnalysisFileError::Corrupt)?;
        let payload_offset = reader.read_u64().map_err(|_| AnalysisFileError::Corrupt)?;
        let payload_end = reader.read_u64().map_err(|_| AnalysisFileError::Corrupt)?;
        let latest_payload_offset = reader.read_u64().map_err(|_| AnalysisFileError::Corrupt)?;
        let latest_payload_len = reader.read_u64().map_err(|_| AnalysisFileError::Corrupt)?;
        let waveform = read_fingerprint(&mut reader)?;
        let beat = read_fingerprint(&mut reader)?;
        reader.finish().map_err(|_| AnalysisFileError::Corrupt)?;
        Ok(Self {
            settled,
            latest_revision,
            source_sample_rate,
            extent,
            chunk_frames,
            chunk_count,
            index_offset,
            payload_offset,
            payload_end,
            latest_payload_offset,
            latest_payload_len,
            fingerprint: AnalysisFingerprint::new(beat.as_deref(), waveform.as_deref()),
        })
    }

    fn encode(&self) -> Result<Bytes, AnalysisFileError> {
        let mut out = BytesMut::with_capacity(consts::HEADER_LEN);
        push_u32(&mut out, Self::VERSION);
        out.put_u8(u8::from(self.settled));
        out.extend_from_slice(&[0; 3]);
        push_u64(&mut out, self.latest_revision);
        push_u32(&mut out, self.source_sample_rate.get());
        out.extend_from_slice(&[0; 4]);
        push_u64(&mut out, self.extent);
        push_u64(&mut out, self.chunk_frames.get());
        push_u64(&mut out, self.chunk_count);
        push_u64(&mut out, self.index_offset);
        push_u64(&mut out, self.payload_offset);
        push_u64(&mut out, self.payload_end);
        push_u64(&mut out, self.latest_payload_offset);
        push_u64(&mut out, self.latest_payload_len);
        push_fingerprint(&mut out, self.fingerprint.waveform())?;
        push_fingerprint(&mut out, self.fingerprint.beat())?;
        if out.len() != consts::HEADER_LEN {
            return Err(AnalysisFileError::Corrupt);
        }
        Ok(out.freeze())
    }

    fn validate(&self, layout: Layout, byte_len: usize) -> Result<(), AnalysisFileError> {
        if self.chunk_count != layout.chunk_count
            || self.index_offset != layout.index_offset
            || self.payload_offset != layout.payload_offset
        {
            return Err(AnalysisFileError::Corrupt);
        }
        let byte_len = u64::try_from(byte_len).map_err(|_| AnalysisFileError::Corrupt)?;
        let latest_end = self
            .latest_payload_offset
            .checked_add(self.latest_payload_len)
            .ok_or(AnalysisFileError::Corrupt)?;
        if self.payload_end != byte_len
            || self.payload_end < self.payload_offset
            || self.latest_payload_len == 0
            || self.latest_payload_offset != self.payload_offset
            || latest_end != self.payload_end
        {
            return Err(AnalysisFileError::Corrupt);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct IndexEntry(bool);

impl IndexEntry {
    fn decode(bytes: &[u8]) -> Result<Self, AnalysisFileError> {
        let mut reader = Reader::new(bytes);
        let entry = Self(reader.read_bool().map_err(|_| AnalysisFileError::Corrupt)?);
        reader.finish().map_err(|_| AnalysisFileError::Corrupt)?;
        Ok(entry)
    }

    fn encode(self) -> [u8; consts::INDEX_ENTRY_LEN] {
        [u8::from(self.0)]
    }

    const fn is_empty(self) -> bool {
        !self.0
    }
}

fn build_update(
    spec: &AnalysisFileSpec,
    existing: Option<&AnalysisFile>,
    progress: &AnalysisProgress,
) -> Result<AnalysisFileUpdate, AnalysisFileError> {
    let analysis = progress.analysis();
    validate_analysis(spec, analysis)?;
    progress.validate_resume()?;
    if progress
        .resume_meta()
        .is_some_and(|(chunk_frames, _)| chunk_frames != spec.chunk_frames)
    {
        return Err(AnalysisFileError::Config);
    }
    let layout = spec.layout()?;
    let (mut header, index, initial) = if let Some(file) = existing {
        if analysis.revision() <= file.header.latest_revision {
            return Err(AnalysisFileError::StaleRevision {
                stored: file.header.latest_revision,
                incoming: analysis.revision(),
            });
        }
        if file.header.settled && !analysis.is_settled() {
            return Err(AnalysisFileError::Config);
        }
        (file.header.clone(), file.index.clone(), None)
    } else {
        let header = Header::base(spec, layout);
        let count = usize::try_from(layout.chunk_count).map_err(|_| AnalysisFileError::TooLarge)?;
        let index: Vec<IndexEntry> = vec![IndexEntry::default(); count];
        let mut initial = BytesMut::from(header.encode()?.as_ref());
        let initial_len =
            usize::try_from(layout.payload_offset).map_err(|_| AnalysisFileError::TooLarge)?;
        initial.resize(initial_len, 0);
        (header, index, Some(initial.freeze()))
    };

    let mut payload = Vec::new();
    progress.write_to(&mut payload)?;
    let payload_len = u64::try_from(payload.len()).map_err(|_| AnalysisFileError::TooLarge)?;
    let payload_offset = layout.payload_offset;
    let final_len = payload_offset
        .checked_add(payload_len)
        .ok_or(AnalysisFileError::TooLarge)?;
    let entry = IndexEntry(true);

    let mut patches: Vec<AnalysisFilePatch> = Vec::new();
    for (index_position, held) in index.iter().copied().enumerate() {
        let id = u64::try_from(index_position).map_err(|_| AnalysisFileError::TooLarge)?;
        let range = chunk_range(spec, id)?;
        let covered = analysis.coverage().covers(&range);
        if !held.is_empty() && !covered {
            return Err(AnalysisFileError::CoverageRegression { chunk: id });
        }
        if held.is_empty() && covered {
            let offset = index_entry_offset(layout, id)?;
            patches.push(AnalysisFilePatch::new(
                offset,
                Bytes::copy_from_slice(&entry.encode()),
            ));
        }
    }

    header.settled = analysis.is_settled();
    header.latest_revision = analysis.revision();
    header.payload_end = final_len;
    header.latest_payload_offset = payload_offset;
    header.latest_payload_len = payload_len;
    patches.push(AnalysisFilePatch::new(0, header.encode()?));

    Ok(AnalysisFileUpdate::new(
        initial,
        AnalysisFileWrite::new(payload_offset, Bytes::from(payload)),
        patches,
        final_len,
    ))
}

fn validate_analysis(
    spec: &AnalysisFileSpec,
    analysis: &TrackAnalysis,
) -> Result<(), AnalysisFileError> {
    if analysis.source_sample_rate() != spec.source_sample_rate
        || analysis.extent() != Some(spec.extent)
        || analysis.fingerprint() != &spec.fingerprint
        || analysis
            .coverage()
            .iter()
            .any(|range| range.end > spec.extent)
    {
        return Err(AnalysisFileError::Config);
    }
    Ok(())
}

fn validate_index_coverage(
    spec: &AnalysisFileSpec,
    index: &[IndexEntry],
    latest: &TrackAnalysis,
) -> Result<(), AnalysisFileError> {
    for (position, entry) in index.iter().copied().enumerate() {
        let id = u64::try_from(position).map_err(|_| AnalysisFileError::Corrupt)?;
        let range = chunk_range(spec, id).map_err(|_| AnalysisFileError::Corrupt)?;
        let covered = latest.coverage().covers(&range);
        if entry.is_empty() != !covered {
            return Err(AnalysisFileError::Corrupt);
        }
    }
    Ok(())
}

fn chunk_range(spec: &AnalysisFileSpec, id: u64) -> Result<Range<u64>, AnalysisFileError> {
    let start = id
        .checked_mul(spec.chunk_frames.get())
        .ok_or(AnalysisFileError::TooLarge)?;
    let end = start
        .saturating_add(spec.chunk_frames.get())
        .min(spec.extent);
    if end <= start {
        return Err(AnalysisFileError::Corrupt);
    }
    Ok(start..start + end - start)
}

fn index_entry_offset(layout: Layout, id: u64) -> Result<u64, AnalysisFileError> {
    let relative = id
        .checked_mul(
            u64::try_from(consts::INDEX_ENTRY_LEN).map_err(|_| AnalysisFileError::TooLarge)?,
        )
        .ok_or(AnalysisFileError::TooLarge)?;
    layout
        .index_offset
        .checked_add(relative)
        .ok_or(AnalysisFileError::TooLarge)
}

fn validate_fingerprint(fingerprint: &AnalysisFingerprint) -> Result<(), AnalysisFileError> {
    for value in [fingerprint.waveform(), fingerprint.beat()] {
        if value.is_some_and(str::is_empty) {
            return Err(AnalysisFileError::Config);
        }
        let len = value.map_or(0, str::len);
        if len > consts::FINGERPRINT_MAX {
            return Err(AnalysisFileError::FingerprintTooLong {
                len,
                max: consts::FINGERPRINT_MAX,
            });
        }
    }
    Ok(())
}

fn push_fingerprint(out: &mut BytesMut, value: Option<&str>) -> Result<(), AnalysisFileError> {
    let bytes = value.map_or(&[][..], str::as_bytes);
    if bytes.len() > consts::FINGERPRINT_MAX {
        return Err(AnalysisFileError::FingerprintTooLong {
            len: bytes.len(),
            max: consts::FINGERPRINT_MAX,
        });
    }
    push_u32(
        out,
        u32::try_from(bytes.len()).map_err(|_| AnalysisFileError::TooLarge)?,
    );
    let start = out.len();
    out.resize(start + consts::FINGERPRINT_MAX, 0);
    out[start..start + bytes.len()].copy_from_slice(bytes);
    Ok(())
}

fn read_fingerprint(reader: &mut Reader<'_>) -> Result<Option<String>, AnalysisFileError> {
    let len = usize::try_from(reader.read_u32().map_err(|_| AnalysisFileError::Corrupt)?)
        .map_err(|_| AnalysisFileError::Corrupt)?;
    let slot = reader
        .read_array::<{ consts::FINGERPRINT_MAX }>()
        .map_err(|_| AnalysisFileError::Corrupt)?;
    if len > consts::FINGERPRINT_MAX || slot[len..].iter().any(|byte| *byte != 0) {
        return Err(AnalysisFileError::Corrupt);
    }
    if len == 0 {
        return Ok(None);
    }
    let value = str::from_utf8(&slot[..len]).map_err(|_| AnalysisFileError::Corrupt)?;
    Ok(Some(value.to_owned()))
}

fn push_u32(out: &mut BytesMut, value: u32) {
    out.put_u32_le(value);
}

fn push_u64(out: &mut BytesMut, value: u64) {
    out.put_u64_le(value);
}

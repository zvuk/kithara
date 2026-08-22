use std::mem::size_of;

use crate::{
    blob::{self, Blob, BlobError, MAX_PREALLOC, Reader, Writer},
    region::GridSegment,
};

struct Consts;

impl Consts {
    /// One frame position on the wire.
    const FRAME_BYTES: usize = size_of::<u64>();
    /// A length prefix on the wire.
    const LEN_PREFIX_BYTES: usize = size_of::<u64>();
    /// `beats`, `downbeats`, `segments` — three length-prefixed lists.
    const LIST_COUNT: usize = 3;
    /// `start_frame`, `end_frame`, `ratio_correction`.
    const SEGMENT_BYTES: usize = size_of::<u64>() * 2 + size_of::<f64>();
    /// Wire/disk format version for the [`BeatGrid`] blob. Bump when the
    /// encoding changes.
    const VERSION: u32 = 1;
}

/// Cleaned beat grid for one track. All positions are source frames
/// (decoder/song time, `PcmMeta.frame_offset` space) — never output/stretched
/// time.
#[derive(Debug, Clone, PartialEq, fieldwork::Fieldwork)]
#[non_exhaustive]
#[fieldwork(get)]
pub struct BeatGrid {
    /// Beat positions in source frames, ascending.
    beats: Vec<u64>,
    /// Downbeat (bar start) positions in source frames, ascending.
    downbeats: Vec<u64>,
    /// Piecewise-constant stretch segments, sorted and non-overlapping.
    segments: Vec<GridSegment>,
    /// Tempo estimated from cleaned beat marks, with a downbeat fallback.
    bpm: f64,
}

impl BeatGrid {
    /// Construct from already-cleaned parts.
    #[must_use]
    pub const fn new(
        bpm: f64,
        beats: Vec<u64>,
        downbeats: Vec<u64>,
        segments: Vec<GridSegment>,
    ) -> Self {
        Self {
            beats,
            downbeats,
            segments,
            bpm,
        }
    }
}

/// Serialize to a versioned little-endian blob: `u32` version, `f64`
/// bpm, then the three length-prefixed position/segment lists.
impl From<&BeatGrid> for Vec<u8> {
    fn from(grid: &BeatGrid) -> Self {
        blob::to_bytes(grid)
    }
}

/// Parse a blob produced by `Vec::<u8>::from(&BeatGrid)`.
///
/// Yields [`BlobError::Version`] on a stale header, [`BlobError::Corrupt`] on a
/// malformed body.
impl TryFrom<&[u8]> for BeatGrid {
    type Error = BlobError;

    fn try_from(bytes: &[u8]) -> Result<Self, BlobError> {
        blob::from_bytes(bytes)
    }
}

impl Blob for BeatGrid {
    const VERSION: u32 = Consts::VERSION;

    fn decode(r: &mut Reader<'_>) -> Result<Self, BlobError> {
        let bpm = read_finite(r)?;
        let beats = r.read_frames()?;
        let downbeats = r.read_frames()?;
        let segment_count = r.read_len()?;
        let mut segments = Vec::with_capacity(segment_count.min(MAX_PREALLOC));
        for _ in 0..segment_count {
            segments.push(GridSegment::new(
                r.read_u64()?,
                r.read_u64()?,
                read_finite(r)?,
            ));
        }
        Ok(Self::new(bpm, beats, downbeats, segments))
    }

    fn encode(&self, w: &mut Writer<'_>) {
        w.reserve(
            size_of::<f64>()
                + Consts::LIST_COUNT * Consts::LEN_PREFIX_BYTES
                + Consts::FRAME_BYTES * (self.beats.len() + self.downbeats.len())
                + Consts::SEGMENT_BYTES * self.segments.len(),
        );
        w.write_f64(self.bpm);
        w.write_frames(&self.beats);
        w.write_frames(&self.downbeats);
        w.write_len(self.segments.len());
        for segment in &self.segments {
            w.write_u64(segment.start_frame());
            w.write_u64(segment.end_frame());
            w.write_f64(segment.ratio_correction());
        }
    }
}

/// Read an `f64`, rejecting non-finite values as corruption.
fn read_finite(r: &mut Reader<'_>) -> Result<f64, BlobError> {
    let value = r.read_f64()?;
    if value.is_finite() {
        Ok(value)
    } else {
        Err(BlobError::Corrupt)
    }
}

#[cfg(test)]
mod bytes_tests {
    use kithara_test_utils::kithara;

    use super::{BeatGrid, BlobError};
    use crate::region::GridSegment;

    fn sample() -> BeatGrid {
        BeatGrid::new(
            123.5,
            vec![0, 22_050, 44_100, 66_150],
            vec![0, 88_200],
            vec![
                GridSegment::new(0, 88_200, 1.02),
                GridSegment::new(88_200, 176_400, 0.98),
            ],
        )
    }

    #[kithara::test]
    fn round_trips() {
        let grid = sample();
        let bytes = Vec::<u8>::from(&grid);
        let back = BeatGrid::try_from(bytes.as_slice()).expect("valid blob round-trips");
        assert_eq!(back, grid);
    }

    #[kithara::test]
    fn degraded_grid_round_trips() {
        let grid = BeatGrid::new(0.0, Vec::new(), Vec::new(), Vec::new());
        let bytes = Vec::<u8>::from(&grid);
        let back = BeatGrid::try_from(bytes.as_slice()).expect("empty blob round-trips");
        assert_eq!(back, grid);
    }

    #[kithara::test]
    fn rejects_wrong_version() {
        let mut bytes = Vec::<u8>::from(&sample());
        bytes[0] = bytes[0].wrapping_add(1);
        assert!(matches!(
            BeatGrid::try_from(bytes.as_slice()),
            Err(BlobError::Version { .. })
        ));
    }

    #[kithara::test]
    fn rejects_corrupt_blobs() {
        let corrupt = |bytes: &[u8]| matches!(BeatGrid::try_from(bytes), Err(BlobError::Corrupt));
        assert!(corrupt(&[0, 0]), "shorter than the version header");

        let mut truncated = Vec::<u8>::from(&sample());
        truncated.pop();
        assert!(corrupt(&truncated), "truncated body");

        let mut trailing = Vec::<u8>::from(&sample());
        trailing.push(0);
        assert!(corrupt(&trailing), "trailing garbage");
    }
}

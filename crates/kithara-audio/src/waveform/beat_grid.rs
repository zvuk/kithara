use crate::blob::{self, Blob, BlobError, MAX_PREALLOC, Reader, Writer};

/// Cleaned beat grid for one track. All positions are source frames
/// (decoder/song time, `PcmMeta.frame_offset` space) — never output/stretched
/// time.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct BeatGrid {
    /// Stable-window tempo of the track, beats per minute.
    pub bpm: f64,
    /// Beat positions in source frames, ascending.
    pub beats: Vec<u64>,
    /// Downbeat (bar start) positions in source frames, ascending.
    pub downbeats: Vec<u64>,
    /// Piecewise-constant stretch segments, sorted and non-overlapping.
    pub segments: Vec<GridSegment>,
}

/// Wire/disk format version for the [`BeatGrid`] blob. Bump when the
/// encoding changes.
const BEAT_GRID_BYTES_VERSION: u32 = 1;

impl BeatGrid {
    /// Construct from already-cleaned parts.
    #[must_use]
    pub fn new(bpm: f64, beats: Vec<u64>, downbeats: Vec<u64>, segments: Vec<GridSegment>) -> Self {
        Self {
            bpm,
            beats,
            downbeats,
            segments,
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
    const VERSION: u32 = BEAT_GRID_BYTES_VERSION;

    fn encode(&self, w: &mut Writer<'_>) {
        w.reserve(
            8 + 3 * 8 + 8 * (self.beats.len() + self.downbeats.len()) + 24 * self.segments.len(),
        );
        w.write_f64(self.bpm);
        w.write_frames(&self.beats);
        w.write_frames(&self.downbeats);
        w.write_len(self.segments.len());
        for segment in &self.segments {
            w.write_u64(segment.start_frame);
            w.write_u64(segment.end_frame);
            w.write_f64(segment.ratio_correction);
        }
    }

    fn decode(r: &mut Reader<'_>) -> Result<Self, BlobError> {
        let bpm = read_finite(r)?;
        let beats = r.read_frames()?;
        let downbeats = r.read_frames()?;
        let segment_count = r.read_len()?;
        let mut segments = Vec::with_capacity(segment_count.min(MAX_PREALLOC));
        for _ in 0..segment_count {
            segments.push(GridSegment {
                start_frame: r.read_u64()?,
                end_frame: r.read_u64()?,
                ratio_correction: read_finite(r)?,
            });
        }
        Ok(Self {
            bpm,
            beats,
            downbeats,
            segments,
        })
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

/// One uniform-tempo region of the grid: `[start_frame, end_frame)` in
/// source frames with a single time-stretch ratio correction.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct GridSegment {
    pub start_frame: u64,
    pub end_frame: u64,
    /// `nominal_bar / fitted_bar`; 1.0 = the region already sits on the grid.
    pub ratio_correction: f64,
}

impl GridSegment {
    /// Construct a segment; see [`BeatGrid::new`] for why this exists.
    #[must_use]
    pub fn new(start_frame: u64, end_frame: u64, ratio_correction: f64) -> Self {
        Self {
            start_frame,
            end_frame,
            ratio_correction,
        }
    }
}

#[cfg(test)]
mod bytes_tests {
    use kithara_test_utils::kithara;

    use super::{BeatGrid, BlobError, GridSegment};

    fn sample() -> BeatGrid {
        BeatGrid {
            bpm: 123.5,
            beats: vec![0, 22_050, 44_100, 66_150],
            downbeats: vec![0, 88_200],
            segments: vec![
                GridSegment {
                    start_frame: 0,
                    end_frame: 88_200,
                    ratio_correction: 1.02,
                },
                GridSegment {
                    start_frame: 88_200,
                    end_frame: 176_400,
                    ratio_correction: 0.98,
                },
            ],
        }
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
        let grid = BeatGrid {
            bpm: 0.0,
            beats: Vec::new(),
            downbeats: Vec::new(),
            segments: Vec::new(),
        };
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

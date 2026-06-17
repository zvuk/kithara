use crate::waveform::GridSegment;

/// Per-region stretch plan: sorted, non-overlapping `[start, end)` segments in
/// source frames (`PcmMeta.frame_offset` space), each with a ratio correction.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct RegionPlan {
    /// Segments sorted by `start_frame`, non-overlapping.
    pub segments: Vec<GridSegment>,
}

/// Validation error for [`RegionPlan::new`].
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum RegionPlanError {
    /// A segment has `start_frame >= end_frame`.
    #[error("region segment {index} is empty or inverted (start >= end)")]
    Inverted { index: usize },
    /// A segment's ratio correction is not a positive finite number.
    #[error("region segment {index} has invalid ratio correction {ratio}")]
    Ratio { index: usize, ratio: f64 },
    /// A segment starts before its predecessor ends (or out of order).
    #[error("region segment {index} overlaps or precedes its predecessor")]
    Overlap { index: usize },
}

impl RegionPlan {
    /// Build a plan from `segments`, validating order and non-overlap.
    ///
    /// # Errors
    /// Returns a [`RegionPlanError`] naming the first offending segment.
    pub fn new(segments: Vec<GridSegment>) -> Result<Self, RegionPlanError> {
        for (index, s) in segments.iter().enumerate() {
            if s.start_frame >= s.end_frame {
                return Err(RegionPlanError::Inverted { index });
            }
            if !s.ratio_correction.is_finite() || s.ratio_correction <= 0.0 {
                return Err(RegionPlanError::Ratio {
                    index,
                    ratio: s.ratio_correction,
                });
            }
            if index > 0 && segments[index - 1].end_frame > s.start_frame {
                return Err(RegionPlanError::Overlap { index });
            }
        }
        Ok(Self { segments })
    }

    /// Resolve the region covering `frame`: a plan segment, or the gap
    /// between segments (correction `1.0`).
    pub(crate) fn region_at(&self, frame: u64) -> ActiveRegion {
        let idx = self.segments.partition_point(|s| s.end_frame <= frame);
        match self.segments.get(idx) {
            Some(s) if s.start_frame <= frame => ActiveRegion {
                start: s.start_frame,
                end: s.end_frame,
                correction: s.ratio_correction,
            },
            Some(s) => ActiveRegion {
                start: idx
                    .checked_sub(1)
                    .map_or(0, |prev| self.segments[prev].end_frame),
                end: s.start_frame,
                correction: 1.0,
            },
            None => ActiveRegion {
                start: self.segments.last().map_or(0, |s| s.end_frame),
                end: u64::MAX,
                correction: 1.0,
            },
        }
    }
}

/// Resolved uniform-ratio span covering one source frame: either a plan
/// segment or a gap between segments.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ActiveRegion {
    pub(crate) correction: f64,
    pub(crate) end: u64,
    pub(crate) start: u64,
}

impl ActiveRegion {
    /// The whole-track region used when no plan is installed.
    pub(crate) const UNBOUNDED: Self = Self {
        start: 0,
        end: u64::MAX,
        correction: 1.0,
    };

    pub(crate) fn contains(&self, frame: u64) -> bool {
        self.start <= frame && frame < self.end
    }
}

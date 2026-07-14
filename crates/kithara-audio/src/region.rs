/// One uniform-tempo region of the grid: `[start_frame, end_frame)` in
/// source frames with a single time-stretch ratio correction.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct GridSegment {
    /// `nominal_bar / fitted_bar`; 1.0 = the region already sits on the grid.
    ratio_correction: f64,
    end_frame: u64,
    start_frame: u64,
}

impl GridSegment {
    /// Construct a segment.
    #[must_use]
    pub fn new(start_frame: u64, end_frame: u64, ratio_correction: f64) -> Self {
        Self {
            ratio_correction,
            end_frame,
            start_frame,
        }
    }

    #[must_use]
    pub fn end_frame(&self) -> u64 {
        self.end_frame
    }

    #[must_use]
    pub fn ratio_correction(&self) -> f64 {
        self.ratio_correction
    }

    #[must_use]
    pub fn start_frame(&self) -> u64 {
        self.start_frame
    }
}

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
            if s.start_frame() >= s.end_frame() {
                return Err(RegionPlanError::Inverted { index });
            }
            if !s.ratio_correction().is_finite() || s.ratio_correction() <= 0.0 {
                return Err(RegionPlanError::Ratio {
                    index,
                    ratio: s.ratio_correction(),
                });
            }
            if index > 0 && segments[index - 1].end_frame() > s.start_frame() {
                return Err(RegionPlanError::Overlap { index });
            }
        }
        Ok(Self { segments })
    }

    /// Resolve the region covering `frame`: a plan segment, or the gap
    /// between segments (correction `1.0`).
    #[must_use]
    pub fn region_at(&self, frame: u64) -> ActiveRegion {
        let idx = self.segments.partition_point(|s| s.end_frame() <= frame);
        match self.segments.get(idx) {
            Some(s) if s.start_frame() <= frame => {
                ActiveRegion::new(s.start_frame(), s.end_frame(), s.ratio_correction())
            }
            Some(s) => ActiveRegion::new(
                idx.checked_sub(1)
                    .map_or(0, |prev| self.segments[prev].end_frame()),
                s.start_frame(),
                1.0,
            ),
            None => ActiveRegion::new(
                self.segments.last().map_or(0, GridSegment::end_frame),
                u64::MAX,
                1.0,
            ),
        }
    }
}

/// Resolved uniform-ratio span covering one source frame: either a plan
/// segment or a gap between segments.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct ActiveRegion {
    correction: f64,
    end: u64,
    start: u64,
}

impl ActiveRegion {
    /// The whole-track region used when no plan is installed.
    pub const UNBOUNDED: Self = Self {
        start: 0,
        end: u64::MAX,
        correction: 1.0,
    };

    #[must_use]
    pub fn new(start: u64, end: u64, correction: f64) -> Self {
        Self {
            correction,
            end,
            start,
        }
    }

    #[must_use]
    pub fn correction(&self) -> f64 {
        self.correction
    }

    #[must_use]
    pub fn end(&self) -> u64 {
        self.end
    }

    #[must_use]
    pub fn start(&self) -> u64 {
        self.start
    }

    #[must_use]
    pub fn contains(&self, frame: u64) -> bool {
        self.start <= frame && frame < self.end
    }
}

#[cfg(test)]
mod tests {
    use super::{GridSegment, RegionPlan, RegionPlanError};

    fn seg(start: u64, end: u64, ratio: f64) -> GridSegment {
        GridSegment::new(start, end, ratio)
    }

    #[test]
    fn plan_rejects_invalid_segments() {
        assert!(matches!(
            RegionPlan::new(vec![seg(10, 10, 1.0)]),
            Err(RegionPlanError::Inverted { index: 0 })
        ));
        assert!(matches!(
            RegionPlan::new(vec![seg(0, 10, 0.0)]),
            Err(RegionPlanError::Ratio { index: 0, .. })
        ));
        assert!(matches!(
            RegionPlan::new(vec![seg(0, 100, 1.0), seg(50, 200, 1.0)]),
            Err(RegionPlanError::Overlap { index: 1 })
        ));
    }

    #[test]
    fn lookup_covers_segments_and_gaps() {
        let plan =
            RegionPlan::new(vec![seg(100, 200, 1.1), seg(300, 400, 0.9)]).expect("valid plan");
        let cases = [
            (0_u64, 0_u64, 100_u64, 1.0),
            (150, 100, 200, 1.1),
            (250, 200, 300, 1.0),
            (350, 300, 400, 0.9),
            (450, 400, u64::MAX, 1.0),
        ];
        for (frame, start, end, correction) in cases {
            let region = plan.region_at(frame);
            assert_eq!((region.start(), region.end()), (start, end));
            assert!((region.correction() - correction).abs() < f64::EPSILON);
            assert!(region.contains(frame));
        }
    }
}

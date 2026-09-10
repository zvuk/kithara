use arc_swap::ArcSwapOption;
use kithara_platform::sync::Arc;

/// One uniform-tempo region of the grid: `[start_frame, end_frame)` in
/// source frames with one asset tempo.
#[derive(Debug, Clone, Copy, PartialEq, fieldwork::Fieldwork)]
#[non_exhaustive]
#[fieldwork(get)]
pub struct GridSegment {
    /// Asset beats per source second.
    beats_per_second: f64,
    end_frame: u64,
    start_frame: u64,
}

impl GridSegment {
    /// Construct a segment.
    #[must_use]
    pub const fn new(start_frame: u64, end_frame: u64, beats_per_second: f64) -> Self {
        Self {
            beats_per_second,
            end_frame,
            start_frame,
        }
    }
}

/// Per-region stretch plan: sorted, non-overlapping `[start, end)` segments in
/// source frames (`AudioChunkInfo.frame_offset` space), each with its asset tempo.
#[derive(Debug, Clone, PartialEq, fieldwork::Fieldwork)]
#[non_exhaustive]
#[fieldwork(get)]
pub struct RegionPlan {
    /// Segments sorted by `start_frame`, non-overlapping.
    segments: Vec<GridSegment>,
}

/// Validation error for [`RegionPlan::new`].
#[non_exhaustive]
#[derive(Debug, PartialEq, thiserror::Error)]
pub enum RegionPlanError {
    /// A segment has `start_frame >= end_frame`.
    #[error("region segment {index} is empty or inverted (start >= end)")]
    Inverted { index: usize },
    /// A segment's tempo is not a positive finite number.
    #[error("region segment {index} has invalid tempo {ratio}")]
    Ratio { index: usize, ratio: f64 },
    /// A segment starts before its predecessor ends (or out of order).
    #[error("region segment {index} overlaps or precedes its predecessor")]
    Overlap { index: usize },
    /// A segment is positioned on the session axis instead of asset frames.
    #[error("region segment {index} is not positioned in asset frames")]
    SessionAxis { index: usize },
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
            if !s.beats_per_second().is_finite() || s.beats_per_second() <= 0.0 {
                return Err(RegionPlanError::Ratio {
                    index,
                    ratio: s.beats_per_second(),
                });
            }
            if index > 0 && segments[index - 1].end_frame() > s.start_frame() {
                return Err(RegionPlanError::Overlap { index });
            }
        }
        Ok(Self { segments })
    }

    /// Resolve the region covering `frame`: a plan segment, or the gap
    /// between segments (unknown tempo).
    #[must_use]
    pub fn region_at(&self, frame: u64) -> ActiveRegion {
        let idx = self.segments.partition_point(|s| s.end_frame() <= frame);
        match self.segments.get(idx) {
            Some(s) if s.start_frame() <= frame => {
                ActiveRegion::new(s.start_frame(), s.end_frame(), Some(s.beats_per_second()))
            }
            Some(s) => ActiveRegion::new(
                idx.checked_sub(1)
                    .map_or(0, |prev| self.segments[prev].end_frame()),
                s.start_frame(),
                None,
            ),
            None => ActiveRegion::new(
                self.segments.last().map_or(0, GridSegment::end_frame),
                u64::MAX,
                None,
            ),
        }
    }
}

/// Live region plan of one resident item, handed from the deck to the
/// renderer that stretches that item. Swapped whole; picked up on the next chunk.
#[derive(Debug, Default)]
pub struct RegionPlanSlot {
    plan: ArcSwapOption<RegionPlan>,
}

impl RegionPlanSlot {
    delegate::delegate! {
        to self.plan {
            /// The installed plan, if any.
            #[must_use]
            #[call(load_full)]
            pub fn load(&self) -> Option<Arc<RegionPlan>>;
            /// Install or clear the plan.
            #[call(store)]
            pub fn install(&self, plan: Option<Arc<RegionPlan>>);
        }
    }
}

/// Resolved uniform-tempo span covering one source frame: either a plan
/// segment or a gap between segments.
#[derive(Debug, Clone, Copy, PartialEq, fieldwork::Fieldwork)]
#[non_exhaustive]
#[fieldwork(get)]
pub struct ActiveRegion {
    #[field(get, copy)]
    beats_per_second: Option<f64>,
    end: u64,
    start: u64,
}

impl ActiveRegion {
    /// The whole-track region used when no plan is installed.
    pub const UNBOUNDED: Self = Self {
        start: 0,
        end: u64::MAX,
        beats_per_second: None,
    };

    #[must_use]
    pub const fn new(start: u64, end: u64, beats_per_second: Option<f64>) -> Self {
        Self {
            beats_per_second,
            end,
            start,
        }
    }

    #[must_use]
    pub const fn contains(&self, frame: u64) -> bool {
        self.start <= frame && frame < self.end
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::{GridSegment, RegionPlan, RegionPlanError};

    fn seg(start: u64, end: u64, ratio: f64) -> GridSegment {
        GridSegment::new(start, end, ratio)
    }

    #[kithara::test(native, flash(false))]
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

    #[kithara::test(native, flash(false))]
    fn lookup_covers_segments_and_gaps() {
        let plan =
            RegionPlan::new(vec![seg(100, 200, 1.1), seg(300, 400, 0.9)]).expect("valid plan");
        let cases = [
            (0_u64, 0_u64, 100_u64, None),
            (150, 100, 200, Some(1.1)),
            (250, 200, 300, None),
            (350, 300, 400, Some(0.9)),
            (450, 400, u64::MAX, None),
        ];
        for (frame, start, end, correction) in cases {
            let region = plan.region_at(frame);
            assert_eq!((region.start(), region.end()), (start, end));
            assert_eq!(region.beats_per_second(), correction);
            assert!(region.contains(frame));
        }
    }
}

use std::num::NonZeroU32;

use arc_swap::ArcSwapOption;
use kithara_platform::sync::Arc;

use crate::{RateTarget, WarpCursor};

/// Immutable manual target paired with one Free map activation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FreeActivation {
    cursor: WarpCursor,
    rate: RateTarget,
}

impl FreeActivation {
    #[must_use]
    pub const fn new(cursor: WarpCursor, rate: RateTarget) -> Self {
        Self { cursor, rate }
    }

    #[must_use]
    pub const fn cursor(self) -> WarpCursor {
        self.cursor
    }

    #[must_use]
    pub const fn rate(self) -> RateTarget {
        self.rate
    }
}

/// One uniform-tempo region of the grid: `[start_frame, end_frame)` in asset
/// frames with one asset tempo.
///
/// The boundaries count frames of the asset itself, so together with the
/// plan's asset rate they name an exact instant in the recording rather than a
/// position on whatever axis a deck happens to render.
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
/// asset frames, each with its asset tempo.
///
/// A segment boundary is the exact ratio of its frame to `asset_rate`, so the
/// plan describes instants of the recording and outlives any output rate. The
/// renderer names the rate its decoded stream carries when it looks a region
/// up, and the boundaries cross to that axis there in exact integer
/// arithmetic.
#[derive(Debug, Clone, PartialEq, fieldwork::Fieldwork)]
#[non_exhaustive]
#[fieldwork(get)]
pub struct RegionPlan {
    /// Returns the sample rate the segment boundaries count frames of.
    #[field(get, copy)]
    asset_rate: NonZeroU32,
    /// Exact source/output relation prepared for this plan.
    #[field(with, option_set_some)]
    activation: Option<WarpCursor>,
    /// Free activation context paired atomically with its map cursor.
    free_activation: Option<FreeActivation>,
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
    /// Build a plan from `segments` measured in frames of `asset_rate`,
    /// validating order and non-overlap.
    ///
    /// # Errors
    /// Returns a [`RegionPlanError`] naming the first offending segment.
    pub fn new(
        asset_rate: NonZeroU32,
        segments: Vec<GridSegment>,
    ) -> Result<Self, RegionPlanError> {
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
        Ok(Self {
            asset_rate,
            activation: None,
            free_activation: None,
            segments,
        })
    }

    /// Resolve the region covering the decoded frame `frame` of a stream
    /// carried at `output_rate`: a plan segment, or the gap between segments
    /// (unknown tempo).
    ///
    /// The query crosses into asset frames and the resolved boundaries cross
    /// back, both as exact integer ratios, so a plan resolves the same instant
    /// of the recording at every output rate.
    #[must_use]
    pub fn region_at(&self, frame: u64, output_rate: NonZeroU32) -> ActiveRegion {
        let asset_frame = self.asset_frame(frame, output_rate);
        let idx = self
            .segments
            .partition_point(|s| s.end_frame() <= asset_frame);
        let region = match self.segments.get(idx) {
            Some(s) if s.start_frame() <= asset_frame => {
                (s.start_frame(), s.end_frame(), Some(s.beats_per_second()))
            }
            Some(s) => (
                idx.checked_sub(1)
                    .map_or(0, |prev| self.segments[prev].end_frame()),
                s.start_frame(),
                None,
            ),
            None => (
                self.segments.last().map_or(0, GridSegment::end_frame),
                u64::MAX,
                None,
            ),
        };
        ActiveRegion::new(
            self.output_frame(region.0, output_rate),
            self.output_frame(region.1, output_rate),
            region.2,
        )
    }

    /// The asset frame a decoded frame of an `output_rate` stream falls in.
    ///
    /// Flooring the exact ratio keeps the comparison against a whole segment
    /// boundary faithful, because a boundary is itself a whole asset frame.
    fn asset_frame(&self, frame: u64, output_rate: NonZeroU32) -> u64 {
        let scaled = u128::from(frame) * u128::from(self.asset_rate.get());
        u64::try_from(scaled / u128::from(output_rate.get())).unwrap_or(u64::MAX)
    }

    /// The first decoded frame of an `output_rate` stream that reaches the
    /// asset frame `frame`.
    ///
    /// Rounding up makes the converted bounds the exact preimage of the asset
    /// span, so a region covers precisely the output frames whose asset
    /// instants lie inside it.
    fn output_frame(&self, frame: u64, output_rate: NonZeroU32) -> u64 {
        let rate = u128::from(self.asset_rate.get());
        let scaled = u128::from(frame) * u128::from(output_rate.get());
        u64::try_from(scaled.div_ceil(rate)).unwrap_or(u64::MAX)
    }

    /// Attach the manual context that becomes authoritative at this Free activation.
    #[must_use]
    pub fn with_free_activation(mut self, cursor: WarpCursor, rate: RateTarget) -> Self {
        self.activation = Some(cursor);
        self.free_activation = Some(FreeActivation::new(cursor, rate));
        self
    }

    /// The immutable Free handoff context, when this plan was installed by Free adoption.
    #[must_use]
    pub const fn free_handoff(&self) -> Option<FreeActivation> {
        self.free_activation
    }
}

/// Live region plan of one resident item, handed from the deck to the
/// renderer that stretches that item. Swapped whole; picked up on the next chunk.
#[derive(Debug, Default)]
pub struct RegionPlanSlot {
    plan: ArcSwapOption<RegionPlan>,
}

impl RegionPlanSlot {
    /// Exact activation carried by the installed plan, if it has one.
    #[must_use]
    pub fn activation(&self) -> Option<WarpCursor> {
        self.load()
            .as_deref()
            .and_then(RegionPlan::activation)
            .copied()
    }

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
    use std::num::NonZeroU32;

    use kithara_platform::sync::Arc;
    use kithara_test_utils::kithara;

    use super::{GridSegment, RegionPlan, RegionPlanError, RegionPlanSlot};
    use crate::{SessionFrame, WarpMap, WarpMapRevision};

    const ASSET_RATE: u32 = 48_000;

    fn asset_rate() -> NonZeroU32 {
        NonZeroU32::new(ASSET_RATE).expect("invariant: fixture asset rate is non-zero")
    }

    fn seg(start: u64, end: u64, ratio: f64) -> GridSegment {
        GridSegment::new(start, end, ratio)
    }

    fn plan_of(segments: Vec<GridSegment>) -> Result<RegionPlan, RegionPlanError> {
        RegionPlan::new(asset_rate(), segments)
    }

    #[kithara::test(native)]
    fn plan_rejects_invalid_segments() {
        assert!(matches!(
            plan_of(vec![seg(10, 10, 1.0)]),
            Err(RegionPlanError::Inverted { index: 0 })
        ));
        assert!(matches!(
            plan_of(vec![seg(0, 10, 0.0)]),
            Err(RegionPlanError::Ratio { index: 0, .. })
        ));
        assert!(matches!(
            plan_of(vec![seg(0, 100, 1.0), seg(50, 200, 1.0)]),
            Err(RegionPlanError::Overlap { index: 1 })
        ));
    }

    #[kithara::test(native)]
    fn a_plan_resolves_the_same_instant_at_every_output_rate() {
        let native_rate = NonZeroU32::new(44_100).expect("invariant: fixture rate is non-zero");
        let plan = RegionPlan::new(native_rate, vec![seg(0, 44_100, 2.0)]).expect("valid plan");

        let native = plan.region_at(0, native_rate);
        let resampled = plan.region_at(0, asset_rate());

        assert_eq!(
            (native.start(), native.end()),
            (0, 44_100),
            "one asset second is one second of a 44.1k stream"
        );
        assert_eq!(
            (resampled.start(), resampled.end()),
            (0, 48_000),
            "the same asset second is one second of a 48k stream"
        );
        assert!(
            resampled.contains(47_999) && !resampled.contains(48_000),
            "the converted bound is the exact preimage of the asset span"
        );
        assert_eq!(
            native.beats_per_second(),
            resampled.beats_per_second(),
            "resampling moves frames, not tempo"
        );
    }

    #[kithara::test(native)]
    fn lookup_covers_segments_and_gaps() {
        let plan = plan_of(vec![seg(100, 200, 1.1), seg(300, 400, 0.9)]).expect("valid plan");
        let cases = [
            (0_u64, 0_u64, 100_u64, None),
            (150, 100, 200, Some(1.1)),
            (250, 200, 300, None),
            (350, 300, 400, Some(0.9)),
            (450, 400, u64::MAX, None),
        ];
        for (frame, start, end, correction) in cases {
            let region = plan.region_at(frame, asset_rate());
            assert_eq!((region.start(), region.end()), (start, end));
            assert_eq!(region.beats_per_second(), correction);
            assert!(region.contains(frame));
        }
    }

    #[kithara::test]
    fn activation_carries_the_exact_source_output_relation() {
        let activation = WarpMap::identity(WarpMapRevision::first()).reanchor(
            24_000,
            SessionFrame::new(48_000),
            crate::SessionBeat::default(),
        );
        let plan = plan_of(vec![seg(0, 96_000, 2.0)])
            .expect("fixture plan")
            .with_activation(activation);

        assert_eq!(plan.activation(), Some(&activation));
    }

    #[kithara::test]
    fn slot_publishes_the_installed_plan_activation() {
        let revision = WarpMapRevision::first();
        let activation = WarpMap::identity(revision).reanchor(
            24_000,
            SessionFrame::new(48_000),
            crate::SessionBeat::default(),
        );
        let plan = plan_of(vec![seg(0, 96_000, 2.0)])
            .expect("fixture plan")
            .with_activation(activation);
        let slot = RegionPlanSlot::default();

        slot.install(Some(Arc::new(plan)));
        assert_eq!(slot.activation(), Some(activation));
        slot.install(None);
        assert_eq!(slot.activation(), None);
    }
}

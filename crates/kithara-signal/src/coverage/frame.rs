use std::ops::Range;

use rangemap::RangeSet;

/// A half-open span of decoded source frames, `[start, end)`, read in frames
/// rather than in endpoints.
pub trait FrameSpan {
    /// Frames the span holds.
    fn frames(&self) -> u64;
}

/// Frame coverage over a set of observed source ranges.
pub trait FrameCoverage {
    /// Whether `span` sits inside one contiguous run. A window is evaluable
    /// exactly when this holds: a span straddling a gap was never observed
    /// whole.
    fn covers(&self, span: &Range<u64>) -> bool;

    /// Total covered frames, counting an overlap once.
    fn frames(&self) -> u64;

    /// Highest covered source frame. At end of stream this is the source
    /// length, which is how a pass learns its extent without a duration.
    fn frontier(&self) -> u64;
}

impl FrameSpan for Range<u64> {
    fn frames(&self) -> u64 {
        self.end.saturating_sub(self.start)
    }
}

impl FrameCoverage for RangeSet<u64> {
    fn covers(&self, span: &Range<u64>) -> bool {
        span.is_empty()
            || self
                .overlapping(span)
                .any(|run| run.start <= span.start && span.end <= run.end)
    }

    delegate::delegate! {
        to self {
            #[expr($.fold(0, |sum, run| sum.saturating_add(run.frames())))]
            #[call(iter)]
            fn frames(&self) -> u64;
            #[expr($.next_back().map_or(0, |run| run.end))]
            #[call(iter)]
            fn frontier(&self) -> u64;
        }
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::{FrameCoverage, Range, RangeSet};
    use crate::{AudioChunkInfo, AudioSpec, consts};

    fn meta(frame_offset: u64, frames: u32) -> AudioChunkInfo {
        AudioChunkInfo {
            spec: AudioSpec {
                channels: 2,
                sample_rate: consts::FRAME_RATE,
            },
            frame_offset,
            frames,
            ..Default::default()
        }
    }

    fn coverage(runs: &[(u64, u64)]) -> RangeSet<u64> {
        let mut out = RangeSet::new();
        for (start, frames) in runs {
            out.insert(*start..start + frames);
        }
        out
    }

    fn gaps(coverage: &RangeSet<u64>, horizon: u64) -> Vec<Range<u64>> {
        coverage.gaps(&(0..horizon)).collect()
    }

    #[kithara::test]
    fn adjacent_chunks_meet_exactly() {
        assert_eq!(meta(0, 1024).frame_range(), 0..1024);
        assert_eq!(
            meta(1024, 1024).frame_range(),
            1024..2048,
            "the second chunk starts where the first ends"
        );

        let coverage = coverage(&[(0, 1024), (1024, 1024)]);
        assert!(coverage.covers(&(0..2048)), "touching runs merge");
        assert_eq!(coverage.frames(), 2048);
    }

    #[kithara::test]
    fn a_span_follows_a_seek_landing() {
        // A seek rewrites `frame_offset` to the landed source frame; the span
        // follows it instead of continuing the previous chunk.
        assert_eq!(meta(441_000, 1024).frame_range(), 441_000..442_024);

        let coverage = coverage(&[(0, 1024), (441_000, 1024)]);
        assert_eq!(coverage.frames(), 2048, "the skipped span stays uncovered");
        assert!(!coverage.covers(&(0..442_024)));
    }

    #[kithara::test]
    fn overlapping_inserts_union() {
        let coverage = coverage(&[(0, 500), (0, 500), (300, 500)]);
        assert_eq!(coverage.frames(), 800, "shared frames counted once");
        assert!(coverage.covers(&(0..800)));
    }

    #[kithara::test]
    fn gapped_inserts_stay_separate_until_filled() {
        let mut coverage = coverage(&[(0, 100), (300, 100)]);
        assert_eq!(coverage.frames(), 200);
        assert!(!coverage.covers(&(0..400)), "the gap is not covered");

        coverage.insert(100..300);
        assert_eq!(coverage.frames(), 400, "filling the gap joins the runs");
        assert!(coverage.covers(&(0..400)));
    }

    #[kithara::test]
    fn covers_needs_one_contiguous_run() {
        let coverage = coverage(&[(0, 100), (200, 100)]);
        assert!(coverage.covers(&(0..50)));
        assert!(coverage.covers(&(200..300)));
        assert!(!coverage.covers(&(50..250)), "spans the gap");
        assert!(coverage.covers(&(10..10)), "nothing to cover");
    }

    #[kithara::test]
    fn the_frontier_is_the_last_covered_frame() {
        assert_eq!(coverage(&[(0, 100), (300, 100)]).frontier(), 400);
        assert_eq!(RangeSet::<u64>::new().frontier(), 0);
    }

    #[kithara::test]
    fn a_hole_between_runs_is_a_gap() {
        assert_eq!(gaps(&coverage(&[(0, 100), (300, 100)]), 400), [100..300]);
    }

    #[kithara::test]
    fn the_tail_below_the_horizon_is_a_gap() {
        assert_eq!(gaps(&coverage(&[(0, 100)]), 250), [100..250]);
    }

    #[kithara::test]
    fn a_run_starting_past_the_start_leaves_the_head_missing() {
        assert_eq!(gaps(&coverage(&[(50, 100)]), 150), [0..50]);
    }

    #[kithara::test]
    fn nothing_is_reported_beyond_the_horizon() {
        // Covered to 400, but only 200 is known to exist.
        assert!(gaps(&coverage(&[(0, 400)]), 200).is_empty());
        // A run wholly past the horizon cannot open a gap behind it.
        assert_eq!(gaps(&coverage(&[(0, 50), (300, 100)]), 200), [50..200]);
    }

    #[kithara::test]
    fn a_run_straddling_the_horizon_closes_the_gap_before_it() {
        // The run starts below the horizon and ends past it, so the gap in
        // front of it stops where the run does, not at the horizon.
        assert_eq!(gaps(&coverage(&[(0, 50), (100, 200)]), 200), [50..100]);
        // The same run with nothing before it leaves only the head missing.
        assert_eq!(gaps(&coverage(&[(150, 100)]), 200), [0..150]);
    }

    #[kithara::test]
    fn full_coverage_has_no_gaps() {
        assert!(gaps(&coverage(&[(0, 400)]), 400).is_empty());
        assert!(gaps(&RangeSet::new(), 0).is_empty());
    }

    #[kithara::test]
    fn an_empty_coverage_is_all_gap() {
        assert_eq!(
            gaps(&RangeSet::new(), 400),
            [0..400],
            "a pass that observed nothing is missing everything it knows of"
        );
    }
}

use std::{collections::BTreeSet, num::NonZeroU32};

use kithara_platform::time::Duration;
use kithara_signal::AudioSpec;

use crate::coverage::{Coverage, FrameRange};

#[derive(Default)]
pub(crate) struct Schedule {
    // WHY: Retire seeks that snap into covered audio or they repeat forever.
    barren: BTreeSet<u64>,
}

impl Schedule {
    pub(crate) fn barren(&mut self, at: u64) {
        self.barren.insert(at);
    }

    fn gap_target(&self, coverage: &Coverage, extent: u64, window: Option<u64>) -> Option<u64> {
        let mut widest: Option<FrameRange> = None;
        for gap in coverage.gaps(extent) {
            if self.barren.contains(&aim(gap, window)) {
                continue;
            }
            if widest.is_none_or(|held| gap.frames() > held.frames()) {
                widest = Some(gap);
            }
        }
        widest.map(|gap| aim(gap, window))
    }

    pub(crate) fn next(
        &self,
        coverage: &Coverage,
        extent: Option<u64>,
        window: Option<u64>,
    ) -> Option<u64> {
        let extent = extent?;
        let Some(window) = window.filter(|window| *window > 0) else {
            return self.gap_target(coverage, extent, None);
        };
        let identities = extent.div_ceil(window);
        let mut regions = 2;

        // WHY: Region centres spread early work before scanning leftovers.
        while regions < identities {
            for region in 0..regions {
                let identity = region_midpoint(identities, regions, region);
                if let Some(at) = self.untouched(coverage, extent, window, identity) {
                    return Some(at);
                }
            }
            regions = regions.saturating_mul(2);
        }

        (0..identities)
            .find_map(|identity| self.untouched(coverage, extent, window, identity))
            .or_else(|| self.gap_target(coverage, extent, Some(window)))
    }

    /// Where a run can continue covered audio instead of opening an island of
    /// its own: in front of the widest gap that has covered audio before it.
    pub(crate) fn extend(&self, coverage: &Coverage, extent: Option<u64>) -> Option<u64> {
        let extent = extent?;
        let mut widest: Option<FrameRange> = None;
        for gap in coverage.gaps(extent) {
            if gap.start() == 0 || self.barren.contains(&gap.start()) {
                continue;
            }
            if widest.is_none_or(|held| gap.frames() > held.frames()) {
                widest = Some(gap);
            }
        }
        widest.map(FrameRange::start)
    }

    fn untouched(
        &self,
        coverage: &Coverage,
        extent: u64,
        window: u64,
        identity: u64,
    ) -> Option<u64> {
        let at = identity * window;
        let frames = window.min(extent.saturating_sub(at));
        (!self.barren.contains(&at)
            && frames > 0
            && !overlaps(coverage, FrameRange::new(at, frames)))
        .then_some(at)
    }
}

#[derive(Default)]
pub(crate) struct Extent {
    reachable: Option<u64>,
    reported: Option<u64>,
}

impl Extent {
    pub(crate) fn frames(&self) -> Option<u64> {
        let reported = self.reported?;
        Some(self.reachable.map_or(reported, |limit| reported.min(limit)))
    }

    pub(crate) fn report(&mut self, duration: Option<Duration>, rate: NonZeroU32) {
        self.reported = self.reported.max(extent_frames(duration, rate));
    }

    pub(crate) const fn restore(frames: u64) -> Self {
        Self {
            reported: Some(frames),
            reachable: None,
        }
    }

    pub(crate) fn unreachable(&mut self, frame: u64) {
        self.reachable = Some(self.reachable.map_or(frame, |limit| limit.min(frame)));
    }
}

fn region_midpoint(identities: u64, regions: u64, region: u64) -> u64 {
    let width = identities / regions;
    let wider = identities % regions;
    let start = region * width + region.min(wider);
    let count = width + u64::from(region < wider);
    start + count / 2
}

fn overlaps(coverage: &Coverage, target: FrameRange) -> bool {
    coverage
        .runs()
        .iter()
        .any(|run| run.start() < target.end() && target.start() < run.end())
}

fn aim(gap: FrameRange, window: Option<u64>) -> u64 {
    let Some(inset) = window
        .filter(|window| gap.frames() > *window)
        .map(|window| (gap.frames() - window) / 2)
    else {
        return gap.start();
    };
    gap.start().saturating_add(inset)
}

pub(crate) fn extent_frames(duration: Option<Duration>, rate: NonZeroU32) -> Option<u64> {
    let frames = AudioSpec::new(1, rate).frames_for(duration?).ok()?.get();
    u64::try_from(frames).ok().filter(|frames| *frames > 0)
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use kithara_platform::time::Duration;
    use kithara_test_utils::kithara;

    use super::{Extent, Schedule};
    use crate::coverage::{Coverage, FrameRange};

    struct Consts;

    impl Consts {
        const EXTENT: u64 = 1000;
        const WINDOW: u64 = 200;
    }

    fn coverage(runs: &[(u64, u64)]) -> Coverage {
        let mut out = Coverage::default();
        for (start, frames) in runs {
            out.insert(FrameRange::new(*start, *frames));
        }
        out
    }

    #[kithara::test]
    fn fixed_chunks_spread_by_progressive_subdivision_before_the_remainder() {
        const CHUNKS: u64 = 16;
        const WINDOW: u64 = 16;
        const EXTENT: u64 = CHUNKS * WINDOW;

        let schedule = Schedule::default();
        let mut covered = Coverage::default();
        let mut identities = Vec::new();
        while let Some(at) = schedule.next(&covered, Some(EXTENT), Some(WINDOW)) {
            identities.push(at / WINDOW);
            covered.insert(FrameRange::new(at, WINDOW));
        }

        assert_eq!(&identities[..2], &[4, 12]);
        assert_eq!(&identities[2..6], &[2, 6, 10, 14]);
        assert_eq!(&identities[6..14], &[1, 3, 5, 7, 9, 11, 13, 15]);

        let mut sorted = identities.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, (0..CHUNKS).collect::<Vec<_>>());
    }

    #[kithara::test]
    fn an_untouched_track_starts_with_the_middle_of_its_first_half() {
        let schedule = Schedule::default();
        assert_eq!(
            schedule.next(
                &Coverage::default(),
                Some(Consts::EXTENT),
                Some(Consts::WINDOW)
            ),
            Some(200),
            "the first run is chunk one, the middle of the first half"
        );
    }

    #[kithara::test]
    fn a_hole_one_run_can_close_is_taken_from_its_start() {
        let schedule = Schedule::default();
        assert_eq!(
            schedule.next(
                &coverage(&[(0, 200), (400, 600)]),
                Some(Consts::EXTENT),
                Some(Consts::WINDOW)
            ),
            Some(200),
            "a run from the middle of this hole would leave its front behind"
        );
    }

    #[kithara::test]
    fn the_wider_of_two_holes_goes_first() {
        let schedule = Schedule::default();
        // Holes of 100 and 300 frames; the second one is the wider.
        let covered = coverage(&[(0, 100), (200, 200), (700, 300)]);
        assert_eq!(
            covered.gaps(Consts::EXTENT).len(),
            2,
            "two holes to choose between"
        );
        assert_eq!(
            schedule.next(&covered, Some(Consts::EXTENT), Some(Consts::WINDOW)),
            Some(400)
        );
    }

    #[kithara::test]
    fn an_unbounded_run_is_aimed_at_the_start_of_its_gap() {
        let schedule = Schedule::default();
        assert_eq!(
            schedule.next(&coverage(&[(600, 400)]), Some(Consts::EXTENT), None),
            Some(0),
            "a run with no window decodes to the end, so only its start matters"
        );
    }

    #[kithara::test]
    fn every_gap_closes_in_a_bounded_number_of_runs() {
        // Each choice covers the window it was aimed at, the way a run does.
        let schedule = Schedule::default();
        let mut covered = Coverage::default();
        let mut runs = 0;

        while let Some(at) = schedule.next(&covered, Some(Consts::EXTENT), Some(Consts::WINDOW)) {
            covered.insert(FrameRange::new(at, Consts::WINDOW));
            runs += 1;
            assert!(
                runs <= 2 * Consts::EXTENT.div_ceil(Consts::WINDOW),
                "a choice that leaves the front of its gap behind never converges: {:?}",
                covered.runs()
            );
        }

        assert!(
            covered.contains(FrameRange::new(0, Consts::EXTENT)),
            "the track is covered, not approached: {:?}",
            covered.gaps(Consts::EXTENT)
        );
    }

    #[kithara::test]
    fn extending_aims_at_the_end_of_covered_audio() {
        let schedule = Schedule::default();
        // Gaps of 200 and 300 frames; only the second has audio before it.
        let covered = coverage(&[(200, 200), (700, 300)]);
        assert_eq!(
            schedule.extend(&covered, Some(Consts::EXTENT)),
            Some(400),
            "the leading gap opens an island, so it is not where a run continues"
        );
        assert_eq!(
            schedule.extend(&coverage(&[(200, 800)]), Some(Consts::EXTENT)),
            None,
            "with only a leading gap there is nothing to continue"
        );
    }

    #[kithara::test]
    fn a_covered_extent_schedules_nothing() {
        let schedule = Schedule::default();
        assert_eq!(
            schedule.next(
                &coverage(&[(0, Consts::EXTENT)]),
                Some(Consts::EXTENT),
                Some(Consts::WINDOW)
            ),
            None
        );
    }

    #[kithara::test]
    fn nothing_is_scheduled_without_an_extent() {
        let schedule = Schedule::default();
        assert_eq!(
            schedule.next(&coverage(&[(0, 200)]), None, Some(Consts::WINDOW)),
            None,
            "a source that reports no duration has no span to place a run in"
        );
    }

    #[kithara::test]
    fn a_position_that_added_nothing_is_not_chosen_again() {
        let mut schedule = Schedule::default();
        let covered = coverage(&[(0, 100), (200, 200), (700, 300)]);
        assert_eq!(
            schedule.next(&covered, Some(Consts::EXTENT), Some(Consts::WINDOW)),
            Some(400)
        );

        // The seek to 400 snapped back into covered audio and added nothing.
        schedule.barren(400);
        assert_eq!(
            schedule.next(&covered, Some(Consts::EXTENT), Some(Consts::WINDOW)),
            Some(450),
            "recovery may try another position, but never repeats the barren one"
        );

        schedule.barren(450);
        assert_eq!(
            schedule.next(&covered, Some(Consts::EXTENT), Some(Consts::WINDOW)),
            Some(100),
            "the next choice comes from the other uncovered range"
        );

        schedule.barren(100);
        assert_eq!(
            schedule.next(&covered, Some(Consts::EXTENT), Some(Consts::WINDOW)),
            None,
            "a pass with nowhere left to reach is finished, not spinning"
        );
    }

    fn rate() -> NonZeroU32 {
        NonZeroU32::new(44_100).expect("test rate is non-zero")
    }

    #[kithara::test]
    fn an_extent_is_measured_on_the_pass_axis() {
        let mut extent = Extent::default();
        assert_eq!(extent.frames(), None, "nothing is reported yet");

        extent.report(Some(Duration::from_secs(2)), rate());
        assert_eq!(extent.frames(), Some(88_200));

        extent.report(None, rate());
        assert_eq!(
            extent.frames(),
            Some(88_200),
            "a live source reports no duration, which retracts nothing"
        );
        extent.report(Some(Duration::ZERO), rate());
        assert_eq!(extent.frames(), Some(88_200));
    }

    #[kithara::test]
    fn a_reported_length_grows_and_a_proved_one_bounds_it() {
        let mut extent = Extent::default();
        extent.report(Some(Duration::from_secs(2)), rate());
        extent.report(Some(Duration::from_secs(4)), rate());
        assert_eq!(
            extent.frames(),
            Some(176_400),
            "the decode path refines a duration upward as it learns more"
        );

        extent.unreachable(100_000);
        assert_eq!(
            extent.frames(),
            Some(100_000),
            "what the source proved it can reach bounds what it claims"
        );
        extent.report(Some(Duration::from_secs(8)), rate());
        assert_eq!(
            extent.frames(),
            Some(100_000),
            "a larger claim does not undo what the source already proved"
        );
    }
}

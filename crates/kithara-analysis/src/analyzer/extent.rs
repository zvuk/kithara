use std::num::NonZeroU32;

use kithara_platform::time::Duration;
use kithara_signal::AudioSpec;

use crate::coverage::FrameRange;

/// The one length a pass works against: unknown until the source stated a
/// length or proved an end; then the stated length bounded by the proven end,
/// and never below the audio the pass was given.
#[derive(Default)]
pub(crate) struct Extent {
    claimed: Option<u64>,
    proved: Option<u64>,
    delivered: u64,
}

impl Extent {
    pub(crate) const fn restore(frames: u64) -> Self {
        Self {
            claimed: Some(frames),
            proved: None,
            delivered: 0,
        }
    }

    pub(crate) fn frames(&self) -> Option<u64> {
        let known = match (self.claimed, self.proved) {
            (Some(claimed), Some(proved)) => claimed.min(proved),
            (claimed, proved) => claimed.or(proved)?,
        };
        Some(known.max(self.delivered))
    }

    /// The reader's stated length, measured on the pass axis.
    pub(crate) fn report(&mut self, duration: Option<Duration>, rate: NonZeroU32) {
        self.claimed = self.claimed.max(frames_for(duration, rate));
    }

    /// Audio the pass was given.
    pub(crate) fn show(&mut self, range: FrameRange) {
        self.delivered = self.delivered.max(range.end());
    }

    /// Where the source ended: end of stream, or a seek answered `PastEof`.
    pub(crate) fn unreachable(&mut self, frame: u64) {
        self.proved = Some(self.proved.map_or(frame, |limit| limit.min(frame)));
    }
}

fn frames_for(duration: Option<Duration>, rate: NonZeroU32) -> Option<u64> {
    let frames = AudioSpec::new(1, rate).frames_for(duration?).ok()?.get();
    u64::try_from(frames).ok().filter(|frames| *frames > 0)
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use kithara_platform::time::Duration;
    use kithara_test_utils::kithara;

    use super::Extent;
    use crate::coverage::FrameRange;

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

    #[kithara::test]
    fn a_range_alone_leaves_the_extent_unknown() {
        let mut extent = Extent::default();
        extent.show(FrameRange::new(0, 1000));
        assert_eq!(extent.frames(), None, "audio alone states no length");

        extent.report(Some(Duration::from_secs(2)), rate());
        extent.show(FrameRange::new(88_000, 1000));
        assert_eq!(
            extent.frames(),
            Some(89_000),
            "the source showed more than it stated"
        );
    }

    #[kithara::test]
    fn an_end_of_stream_at_frontier_zero_leaves_the_extent_at_the_delivered_audio() {
        let mut extent = Extent::default();
        extent.show(FrameRange::new(0, 8192));
        extent.show(FrameRange::new(16_384, 8192));
        assert_eq!(extent.frames(), None);

        extent.unreachable(0);
        assert_eq!(
            extent.frames(),
            Some(24_576),
            "a reader that delivered nothing proves nothing about the audio the pass was given"
        );
    }

    #[kithara::test]
    fn a_proof_below_the_delivered_frontier_does_not_undercut_it() {
        let mut extent = Extent::default();
        extent.report(Some(Duration::from_secs(2)), rate());
        extent.show(FrameRange::new(88_000, 1000));

        extent.unreachable(88_500);
        assert_eq!(
            extent.frames(),
            Some(89_000),
            "audio that was delivered exists, whatever the source proved after it"
        );
    }

    #[kithara::test]
    fn an_end_of_stream_is_the_extent_of_a_source_that_claimed_nothing() {
        let mut extent = Extent::default();
        extent.unreachable(50_000);
        assert_eq!(extent.frames(), Some(50_000));
    }
}

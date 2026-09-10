use num_traits::cast::AsPrimitive;

use super::SegmentSet;
use crate::{GridSegment, MapPosition, RegionPlan, RegionPlanError};

impl SegmentSet {
    /// Freezes asset tempos independently of the deck that will play them.
    ///
    /// # Errors
    ///
    /// Returns [`RegionPlanError::SessionAxis`] for a session-positioned set
    /// and the [`RegionPlan`] validation errors for a degenerate tempo.
    pub fn region_plan(&self) -> Result<RegionPlan, RegionPlanError> {
        let planned = self
            .segments()
            .iter()
            .enumerate()
            .map(|(index, segment)| {
                let (MapPosition::Asset(start), MapPosition::Asset(end)) =
                    (segment.start_position(), segment.end_position())
                else {
                    return Err(RegionPlanError::SessionAxis { index });
                };
                let beats_per_second = segment
                    .tempo(self.axis())
                    .map_or(0.0, |tempo| f64::from(tempo) / 60.0);
                Ok(GridSegment::new(
                    f64::from(start).as_(),
                    f64::from(end).as_(),
                    beats_per_second,
                ))
            })
            .collect::<Result<Vec<_>, _>>()?;
        RegionPlan::new(planned)
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use kithara_test_utils::kithara;

    use crate::{
        AssetAxis, AssetFrame, BeatEvidence, BeatMarker, BeatOrdinal, FrameUncertainty, MapAxis,
        MapPosition, MapSegment, RegionPlanError, SegmentFacts, SegmentSet, SessionAxis,
        SessionEpoch, SessionFrame,
    };

    struct Consts;

    impl Consts {
        const FRAME_COUNT: u64 = 96_000;
        const SAMPLE_RATE: u32 = 48_000;
        const SEGMENT_END: u32 = 48_000;
    }

    fn sample_rate() -> NonZeroU32 {
        NonZeroU32::new(Consts::SAMPLE_RATE).expect("invariant: fixture sample rate is non-zero")
    }

    fn marker(position: MapPosition, ordinal: i64) -> BeatMarker {
        BeatMarker::new(
            position,
            Some(BeatOrdinal::new(ordinal)),
            BeatEvidence::Observed,
            FrameUncertainty::ZERO,
        )
    }

    fn asset_position(frame: f64) -> MapPosition {
        MapPosition::Asset(
            AssetFrame::new(frame).expect("invariant: fixture asset frame is finite"),
        )
    }

    fn segment(start: MapPosition, end: MapPosition, beats: i64) -> MapSegment {
        MapSegment::new(
            marker(start, 0),
            marker(end, beats),
            SegmentFacts::new(BeatEvidence::Observed, FrameUncertainty::ZERO, None),
        )
        .expect("invariant: fixture markers form an increasing affine relation")
    }

    #[kithara::test]
    fn an_asset_set_preserves_each_segments_tempo() {
        let axis = MapAxis::Asset(AssetAxis::new(sample_rate(), Consts::FRAME_COUNT));
        let set = SegmentSet::new(
            axis,
            vec![segment(
                asset_position(0.0),
                asset_position(f64::from(Consts::SEGMENT_END)),
                2,
            )],
        )
        .expect("invariant: one bounded segment is a valid set");

        let plan = set.region_plan().expect("an asset set yields a plan");

        let [region] = plan.segments()[..] else {
            panic!("one segment plans one region, got {plan:?}");
        };
        assert_eq!(region.start_frame(), 0);
        assert_eq!(region.end_frame(), u64::from(Consts::SEGMENT_END));
        assert!(
            (region.beats_per_second() - 2.0).abs() < f64::EPSILON,
            "120 bpm is 2 beats per second, got {}",
            region.beats_per_second()
        );
    }

    #[kithara::test]
    fn a_session_set_has_no_region_plan() {
        let axis = MapAxis::Session(SessionAxis::new(sample_rate(), SessionEpoch::new(0)));
        let set = SegmentSet::new(
            axis,
            vec![segment(
                MapPosition::Session(SessionFrame::new(0)),
                MapPosition::Session(SessionFrame::new(48_000)),
                2,
            )],
        )
        .expect("invariant: one session segment is a valid set");

        assert_eq!(
            set.region_plan().unwrap_err(),
            RegionPlanError::SessionAxis { index: 0 }
        );
    }
}

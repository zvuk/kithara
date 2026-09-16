use super::{
    BeatEstimate, BeatGridId, BeatGridQuery, BeatGridRegion, BeatGridRevision, BeatGridStamp,
    BeatGridState, BeatGridUnavailable, BeatGridView,
};
use crate::{
    Beat, BeatEvidence, BeatsPerMinute, FrameUncertainty, MapAxis, MapPoint, MapPosition, Meter,
    MeterFacts, SessionAnchor, SessionAxis, SessionBeat, SessionEpoch,
};

const SECONDS_PER_MINUTE: f64 = 60.0;

/// Immutable mathematical view of a live session clock.
#[derive(Debug)]
pub(super) struct SessionGridView {
    id: BeatGridId,
    revision: BeatGridRevision,
    meter: Option<MeterFacts>,
    anchor: SessionAnchor,
    axis: SessionAxis,
}

impl SessionGridView {
    pub(super) fn new(
        id: BeatGridId,
        revision: BeatGridRevision,
        epoch: SessionEpoch,
        anchor: SessionAnchor,
        meter: Option<MeterFacts>,
    ) -> Self {
        Self {
            id,
            revision,
            axis: SessionAxis::new(anchor.sample_rate(), epoch),
            anchor,
            meter,
        }
    }

    fn stale<T>(&self, given: BeatGridStamp) -> Option<BeatGridQuery<T>> {
        let expected = self.stamp();
        (given != expected).then_some(BeatGridQuery::Stale { expected, given })
    }
}

impl BeatGridView for SessionGridView {
    fn axis(&self) -> MapAxis {
        MapAxis::Session(self.axis)
    }

    fn beat_at(
        &self,
        position: MapPoint<MapPosition>,
    ) -> BeatGridQuery<BeatEstimate<MapPoint<Beat>>> {
        if let Some(stale) = self.stale(position.stamp()) {
            return stale;
        }
        let MapPosition::Session(frame) = *position.value() else {
            return BeatGridQuery::Unavailable(BeatGridUnavailable::AxisMismatch);
        };
        let Ok(session_beat) = self.anchor.beat_at(frame) else {
            return BeatGridQuery::OutsideDomain;
        };
        let Ok(beat) = Beat::new(f64::from(session_beat)) else {
            return BeatGridQuery::OutsideDomain;
        };
        BeatGridQuery::Resolved(BeatEstimate::new(
            MapPoint::new(self.stamp(), beat),
            BeatEvidence::Declared,
            FrameUncertainty::ZERO,
            self.stamp(),
        ))
    }

    fn id(&self) -> BeatGridId {
        self.id
    }

    fn meter_at(&self, beat: MapPoint<Beat>) -> BeatGridQuery<BeatEstimate<Meter>> {
        if let Some(stale) = self.stale(beat.stamp()) {
            return stale;
        }
        let Some(meter) = self.meter else {
            return BeatGridQuery::Unavailable(BeatGridUnavailable::NoMeter);
        };
        let (value, evidence, uncertainty) = meter.into_parts();
        BeatGridQuery::Resolved(BeatEstimate::new(
            value,
            evidence,
            uncertainty,
            self.stamp(),
        ))
    }

    fn position_at(
        &self,
        beat: MapPoint<Beat>,
    ) -> BeatGridQuery<BeatEstimate<MapPoint<MapPosition>>> {
        if let Some(stale) = self.stale(beat.stamp()) {
            return stale;
        }
        let Ok(session_beat) = SessionBeat::new(f64::from(*beat.value())) else {
            return BeatGridQuery::OutsideDomain;
        };
        let Ok(frame) = self.anchor.frame_at(session_beat) else {
            return BeatGridQuery::OutsideDomain;
        };
        let Ok(rounded_beat) = self.anchor.beat_at(frame) else {
            return BeatGridQuery::OutsideDomain;
        };
        let residual_frames = ((f64::from(session_beat) - f64::from(rounded_beat))
            / self.anchor.beats_per_frame())
        .abs();
        let Ok(uncertainty) = FrameUncertainty::new(residual_frames) else {
            return BeatGridQuery::OutsideDomain;
        };
        BeatGridQuery::Resolved(BeatEstimate::new(
            MapPoint::new(self.stamp(), MapPosition::Session(frame)),
            BeatEvidence::Declared,
            uncertainty,
            self.stamp(),
        ))
    }

    fn region_at(&self, position: MapPoint<MapPosition>) -> BeatGridQuery<BeatGridRegion> {
        if let Some(stale) = self.stale(position.stamp()) {
            return stale;
        }
        if !matches!(position.value(), MapPosition::Session(_)) {
            return BeatGridQuery::Unavailable(BeatGridUnavailable::AxisMismatch);
        }
        BeatGridQuery::Resolved(BeatGridRegion::Unbounded)
    }

    fn revision(&self) -> BeatGridRevision {
        self.revision
    }

    fn state(&self) -> BeatGridState {
        BeatGridState::Live
    }

    fn tempo_at(
        &self,
        position: MapPoint<MapPosition>,
    ) -> BeatGridQuery<BeatEstimate<BeatsPerMinute>> {
        if let Some(stale) = self.stale(position.stamp()) {
            return stale;
        }
        let MapPosition::Session(frame) = *position.value() else {
            return BeatGridQuery::Unavailable(BeatGridUnavailable::AxisMismatch);
        };
        let bpm = self.anchor.tempo_at(frame) * SECONDS_PER_MINUTE;
        let Ok(tempo) = BeatsPerMinute::try_from(bpm) else {
            return BeatGridQuery::Unavailable(BeatGridUnavailable::NoGeometry);
        };
        BeatGridQuery::Resolved(BeatEstimate::new(
            tempo,
            BeatEvidence::Declared,
            FrameUncertainty::ZERO,
            self.stamp(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use kithara_test_utils::kithara;

    use super::SessionGridView;
    use crate::{
        Beat, BeatEvidence, BeatGridId, BeatGridQuery, BeatGridRevision, BeatGridView, BeatOrdinal,
        FrameUncertainty, MapPoint, MapPosition, Meter, MeterFacts, SessionAnchor, SessionAxis,
        SessionBeat, SessionEpoch, SessionFrame,
    };

    #[kithara::test]
    fn session_grid_round_trips_beats_and_preserves_declared_meter() {
        for (sample_rate, beats_per_bar, downbeat, next_bar) in
            [(44_100, 3, 2, 5.0), (48_000, 7, -1, 6.0)]
        {
            let anchor = SessionAnchor::new(
                SessionFrame::new(1_000),
                SessionBeat::new(2.0).expect("finite anchor beat"),
                2.0,
                SessionAxis::new(
                    NonZeroU32::new(sample_rate).expect("sample rate"),
                    SessionEpoch::new(0),
                ),
            )
            .expect("session anchor");
            let meter = Meter::new(beats_per_bar)
                .expect("meter")
                .with_downbeat(BeatOrdinal::new(downbeat));
            let view = SessionGridView::new(
                BeatGridId::allocate().expect("grid id"),
                BeatGridRevision::first(),
                SessionEpoch::new(3),
                anchor,
                Some(MeterFacts::new(
                    meter,
                    BeatEvidence::Declared,
                    FrameUncertainty::ZERO,
                )),
            );

            for beat in [1.5, 2.0, 3.0, next_bar] {
                let beat = Beat::new(beat).expect("finite beat");
                let position = match view.position_at(MapPoint::new(view.stamp(), beat)) {
                    BeatGridQuery::Resolved(position) => position,
                    other => panic!("beat must resolve, got {other:?}"),
                };
                let round_trip = match view.beat_at(*position.value()) {
                    BeatGridQuery::Resolved(beat) => beat,
                    other => panic!("position must resolve, got {other:?}"),
                };
                assert_eq!(*round_trip.value().value(), beat);
            }

            let queried = match view.meter_at(MapPoint::new(
                view.stamp(),
                Beat::new(2.0).expect("finite beat"),
            )) {
                BeatGridQuery::Resolved(meter) => meter,
                other => panic!("meter must resolve, got {other:?}"),
            };
            assert_eq!(*queried.value(), meter);
        }
    }

    #[kithara::test]
    fn a_session_grid_reports_the_tempo_playing_at_the_queried_frame() {
        let anchor = SessionAnchor::new(
            SessionFrame::new(0),
            SessionBeat::new(0.0).expect("finite beat"),
            2.0,
            SessionAxis::new(
                NonZeroU32::new(48_000).expect("sample rate"),
                SessionEpoch::new(0),
            ),
        )
        .expect("session anchor")
        .retarget(SessionFrame::new(0), 3.0, 0.005)
        .expect("a positive target");
        let view = SessionGridView::new(
            BeatGridId::allocate().expect("grid id"),
            BeatGridRevision::first(),
            SessionEpoch::new(0),
            anchor,
            None,
        );
        let at = |frame| {
            let BeatGridQuery::Resolved(estimate) = view.tempo_at(MapPoint::new(
                view.stamp(),
                MapPosition::Session(SessionFrame::new(frame)),
            )) else {
                panic!("a live session grid resolves its tempo");
            };
            f64::from(*estimate.value())
        };

        assert!(
            (at(0) - 120.0).abs() < 1e-9,
            "the approach starts at the tempo already playing, got {}",
            at(0)
        );
        assert!(
            (at(48_000) - 180.0).abs() < 1e-9,
            "a second later the approach has reached its target, got {}",
            at(48_000)
        );
    }

    #[kithara::test]
    fn meterless_session_grid_does_not_invent_bar_phase() {
        let anchor = SessionAnchor::new(
            SessionFrame::new(0),
            SessionBeat::new(0.0).expect("finite beat"),
            2.0,
            SessionAxis::new(
                NonZeroU32::new(48_000).expect("sample rate"),
                SessionEpoch::new(0),
            ),
        )
        .expect("session anchor");
        let view = SessionGridView::new(
            BeatGridId::allocate().expect("grid id"),
            BeatGridRevision::first(),
            SessionEpoch::new(0),
            anchor,
            None,
        );

        assert!(matches!(
            view.meter_at(MapPoint::new(
                view.stamp(),
                Beat::new(0.0).expect("finite beat")
            )),
            BeatGridQuery::Unavailable(_)
        ));
        assert!(matches!(
            view.beat_at(MapPoint::new(
                view.stamp(),
                MapPosition::Session(SessionFrame::new(-12_000))
            )),
            BeatGridQuery::Resolved(_)
        ));
    }
}

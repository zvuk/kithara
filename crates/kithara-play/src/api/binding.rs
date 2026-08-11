use std::num::NonZeroU32;

use kithara_audio::{
    BeatMapError, SessionBeat, SourceFrame, TrackBeat, TrackBeatMap, analysis::TrackAnalysis,
};
use kithara_events::PlaybackDirection;

/// A track cannot participate in session synchronization.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum SyncUnavailable {
    /// The track has no valid analysed beat-to-source map.
    #[error("track beat map unavailable: {reason}")]
    BeatMap {
        #[source]
        reason: BeatMapError,
    },
    /// Composing the binding anchors produced a non-finite coordinate.
    #[error("binding coordinate overflow")]
    CoordinateOverflow,
}

impl From<BeatMapError> for SyncUnavailable {
    fn from(reason: BeatMapError) -> Self {
        Self::BeatMap { reason }
    }
}

/// Immutable relationship between one session beat anchor and one analysed track.
#[derive(Clone, Debug, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(get)]
#[non_exhaustive]
pub struct TrackBinding {
    /// Audible direction encoded by this binding.
    #[field(get, copy)]
    direction: PlaybackDirection,
    /// Session beat anchoring this binding.
    #[field(get, copy)]
    session_anchor: SessionBeat,
    /// Track beat anchoring this binding.
    #[field(get, copy)]
    track_anchor: TrackBeat,
    /// Immutable analysed map used by this binding.
    map: TrackBeatMap,
}

impl TrackBinding {
    /// Builds a binding from real analysed markers.
    pub fn new(
        analysis: &TrackAnalysis,
        host_sample_rate: NonZeroU32,
        session_anchor: SessionBeat,
        track_anchor: TrackBeat,
        direction: PlaybackDirection,
    ) -> Result<Self, SyncUnavailable> {
        Ok(Self {
            direction,
            session_anchor,
            track_anchor,
            map: TrackBeatMap::new(analysis, host_sample_rate)?,
        })
    }

    /// Resolves a session beat through the binding and analysed source map.
    ///
    /// Returns `Ok(None)` when the resolved track beat is outside the analysed
    /// marker domain.
    pub fn source_frame_at(
        &self,
        session_beat: SessionBeat,
    ) -> Result<Option<SourceFrame>, SyncUnavailable> {
        Ok(self.map.source_frame_at(self.track_beat_at(session_beat)?))
    }

    /// Returns the track beat corresponding to this session beat.
    pub fn track_beat_at(&self, session_beat: SessionBeat) -> Result<TrackBeat, SyncUnavailable> {
        let delta = f64::from(session_beat) - f64::from(self.session_anchor);
        let anchor = f64::from(self.track_anchor);
        let value = match self.direction {
            PlaybackDirection::Forward => anchor + delta,
            PlaybackDirection::Reverse => anchor - delta,
        };
        TrackBeat::new(value).map_err(|_| SyncUnavailable::CoordinateOverflow)
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use kithara_audio::{
        BeatGrid, BeatMapError, SessionBeat, SourceFrame, TrackBeat, analysis::TrackAnalysis,
    };
    use kithara_events::PlaybackDirection;
    use kithara_test_utils::kithara;

    use super::{SyncUnavailable, TrackBinding};

    fn sample_rate() -> NonZeroU32 {
        NonZeroU32::new(48_000).expect("invariant: fixture sample rate is non-zero")
    }

    fn analysis_with_grid() -> TrackAnalysis {
        TrackAnalysis::with_source_rate(
            Some(BeatGrid::new(
                120.0,
                vec![0, 48_000, 96_000],
                vec![0],
                Vec::new(),
            )),
            None,
            144_000,
            sample_rate(),
        )
    }

    fn session_beat(value: f64) -> SessionBeat {
        SessionBeat::new(value).expect("invariant: fixture session beat is finite")
    }

    fn track_beat(value: f64) -> TrackBeat {
        TrackBeat::new(value).expect("invariant: fixture track beat is finite")
    }

    fn binding(direction: PlaybackDirection) -> TrackBinding {
        TrackBinding::new(
            &analysis_with_grid(),
            sample_rate(),
            session_beat(10.0),
            track_beat(1.0),
            direction,
        )
        .expect("invariant: fixture beat map is valid")
    }

    #[kithara::test]
    fn forward_binding_increases_track_beats() {
        let binding = binding(PlaybackDirection::Forward);

        assert_eq!(
            binding.track_beat_at(session_beat(9.5)),
            Ok(track_beat(0.5))
        );
        assert_eq!(
            binding.track_beat_at(session_beat(10.5)),
            Ok(track_beat(1.5))
        );
    }

    #[kithara::test]
    fn reverse_binding_decreases_track_beats() {
        let binding = binding(PlaybackDirection::Reverse);

        assert_eq!(
            binding.track_beat_at(session_beat(9.5)),
            Ok(track_beat(1.5))
        );
        assert_eq!(
            binding.track_beat_at(session_beat(10.5)),
            Ok(track_beat(0.5))
        );
    }

    #[kithara::test]
    fn a_beat_before_the_sample_start_resolves_to_no_source_frame() {
        let binding = binding(PlaybackDirection::Forward);

        assert_eq!(binding.source_frame_at(session_beat(8.0)), Ok(None));
    }

    /// The map is seeded to the sample's extent, so the last frame of the
    /// record is a position a bound deck can be at — it is not outside.
    #[kithara::test]
    fn the_beat_the_sample_ends_on_resolves_to_its_last_source_frame() {
        let binding = binding(PlaybackDirection::Forward);
        let end = SourceFrame::new(144_000.0).expect("invariant: the fixture extent is finite");

        assert_eq!(binding.source_frame_at(session_beat(12.0)), Ok(Some(end)));
    }

    #[kithara::test]
    fn a_beat_past_the_sample_end_resolves_to_no_source_frame() {
        let binding = binding(PlaybackDirection::Forward);

        assert_eq!(binding.source_frame_at(session_beat(13.0)), Ok(None));
    }

    #[kithara::test]
    fn missing_beat_grid_is_unavailable() {
        let analysis = TrackAnalysis::with_source_rate(None, None, 0, sample_rate());

        assert!(matches!(
            TrackBinding::new(
                &analysis,
                sample_rate(),
                session_beat(0.0),
                track_beat(0.0),
                PlaybackDirection::Forward,
            ),
            Err(SyncUnavailable::BeatMap {
                reason: BeatMapError::Missing
            })
        ));
    }

    #[kithara::test]
    fn non_finite_composition_is_coordinate_overflow() {
        let binding = TrackBinding::new(
            &analysis_with_grid(),
            sample_rate(),
            session_beat(-f64::MAX),
            track_beat(0.0),
            PlaybackDirection::Forward,
        )
        .expect("invariant: fixture beat map is valid");

        assert_eq!(
            binding.track_beat_at(session_beat(f64::MAX)),
            Err(SyncUnavailable::CoordinateOverflow)
        );
    }
}

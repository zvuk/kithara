use std::num::NonZeroU32;

use kithara_audio::{BeatMapError, SourceFrame, TrackBeat, TrackBeatMap, analysis::TrackAnalysis};
use kithara_events::PlaybackDirection;

use super::SessionBeat;

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
        let delta = session_beat.get() - self.session_anchor.get();
        let value = match self.direction {
            PlaybackDirection::Forward => self.track_anchor.get() + delta,
            PlaybackDirection::Reverse => self.track_anchor.get() - delta,
        };
        TrackBeat::new(value).map_err(|_| SyncUnavailable::CoordinateOverflow)
    }
}

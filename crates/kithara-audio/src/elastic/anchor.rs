use kithara_events::PlaybackDirection;
use kithara_stretch::{ElasticCursor, ElasticError};

/// Transport-neutral source anchor used to prime or relocate exact-span playback.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct ElasticAnchor {
    cursor: ElasticCursor,
    direction: PlaybackDirection,
    source_frames_per_output: f64,
}

impl ElasticAnchor {
    #[must_use]
    pub const fn cursor(self) -> ElasticCursor {
        self.cursor
    }

    #[must_use]
    pub const fn direction(self) -> PlaybackDirection {
        self.direction
    }

    #[must_use]
    pub const fn source_frames_per_output(self) -> f64 {
        self.source_frames_per_output
    }
}

impl TryFrom<(f64, f64, PlaybackDirection)> for ElasticAnchor {
    type Error = ElasticError;

    fn try_from(
        (source_frame, source_frames_per_output, direction): (f64, f64, PlaybackDirection),
    ) -> Result<Self, Self::Error> {
        if !source_frames_per_output.is_finite() || source_frames_per_output <= 0.0 {
            return Err(ElasticError::InvalidRate(source_frames_per_output));
        }
        Ok(Self {
            direction,
            source_frames_per_output,
            cursor: ElasticCursor::try_from(source_frame)?,
        })
    }
}

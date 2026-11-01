use kithara_beat::BeatGridModel;
use kithara_play::{Outbox, PlayError, Position, Track, TrackSnapshot, TrackStatus};
use kithara_signal::SessionFrame;
use kithara_sync::{SourceGrid, speed_curve};
use num_traits::ToPrimitive;

use super::{Linked, LinkedPlayer};

pub(super) fn cue_position(track: &TrackSnapshot) -> Position {
    if matches!(track.status, TrackStatus::Ended { .. }) {
        Position::ZERO
    } else {
        track.position
    }
}

impl<P> Linked<P> {
    pub(super) fn earliest<S>(&self, out: &Outbox<'_, S>) -> Result<SessionFrame, PlayError>
    where
        P: Track<S>,
    {
        let pass = out.pass().ok_or(PlayError::Untimed)?;
        Ok(pass.now + self.lead(pass.delivery).unwrap_or(pass.delivery))
    }

    pub(super) fn planned<S>(&self, at: SessionFrame) -> Result<(Position, f64), PlayError>
    where
        P: Track<S>,
    {
        self.inner.planned(at, self.host.sample_rate())
    }

    pub(super) fn jump_frame<S>(&self, out: &Outbox<'_, S>) -> Result<SessionFrame, PlayError>
    where
        P: Track<S>,
    {
        Ok(self.earliest(out)? + self.inner.snapshot().as_ref().declick)
    }

    pub(super) fn source(&self, grid: &BeatGridModel) -> Result<SourceGrid, PlayError> {
        let rate = self.host.sample_rate();
        // Rounded up so a beat on the measured duration stays inside the extent.
        let extent = grid
            .geometry()
            .duration()
            .map(|seconds| {
                (seconds * f64::from(rate.get()))
                    .ceil()
                    .to_u64()
                    .ok_or(PlayError::Untimed)
            })
            .transpose()?;
        SourceGrid::new(
            grid.geometry(),
            rate,
            extent,
            matches!(grid.as_raw().state, kithara_beat::BeatGridState::Final) && extent.is_some(),
        )
        .map_err(|error| PlayError::Internal(error.to_string()))
    }

    pub(super) fn curve(
        &self,
        grid: &SourceGrid,
        position: Position,
        at: SessionFrame,
        landing: Option<(Position, SessionFrame)>,
    ) -> Result<kithara_sync::SpeedCurve, PlayError> {
        speed_curve(&self.host, grid, position, at, landing).ok_or_else(|| {
            let rate = f64::from(self.host.sample_rate().get());
            let track = grid.bpm_at_frame(position.as_secs_f64() * rate);
            let host = self.host.anchor_at(at).tempo_at(at) * 60.0;
            PlayError::Internal(format!(
                "no host speed law: track at {track:?} BPM, host at {host} BPM, position {position:?}"
            ))
        })
    }
}

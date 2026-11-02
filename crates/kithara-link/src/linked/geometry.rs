use kithara_beat::BeatGridModel;
use kithara_play::{Outbox, PlayError, Position, Track, TrackSnapshot, TrackStatus};
use kithara_signal::{FrameCount, SessionFrame};
use kithara_sync::{SourceGrid, boundary, speed_curve};
use num_traits::ToPrimitive;

use super::{Linked, LinkedPlayer};
use crate::covers;

pub(super) fn cue_position(track: &TrackSnapshot) -> Position {
    if matches!(track.status, TrackStatus::Ended { .. }) {
        Position::ZERO
    } else {
        track.position
    }
}

/// Where a transport command lands: a position the grid covers stays, any
/// other moves to the first boundary at or after it.
pub(super) fn placement(grid: &BeatGridModel, position: Position) -> Option<Position> {
    if covers(grid.geometry(), position) {
        Some(position)
    } else {
        boundary(grid.geometry(), position)
    }
}

/// The last beat of a grid no later answer extends.
pub(super) fn last_beat(grid: &BeatGridModel) -> Option<Position> {
    let last = grid.geometry().beats().last().filter(|_| is_final(grid))?;
    Position::try_from_secs_f64(last.at).ok()
}

/// A final grid is the last answer for its track, so what it leaves out stays out.
pub(super) fn is_final(grid: &BeatGridModel) -> bool {
    matches!(grid.as_raw().state, kithara_beat::BeatGridState::Final)
}

impl<P> Linked<P> {
    pub(super) fn earliest<S>(&self, out: &Outbox<'_, S>) -> Result<SessionFrame, PlayError>
    where
        P: Track<S>,
    {
        let pass = out.pass().ok_or(PlayError::Untimed)?;
        Ok(pass.now + self.lead(pass.delivery).unwrap_or(pass.delivery))
    }

    /// The earliest frame a Start can land on; a silent deck's takes no lane.
    pub(super) fn earliest_start<S>(&self, out: &Outbox<'_, S>) -> Result<SessionFrame, PlayError>
    where
        P: Track<S>,
    {
        let pass = out.pass().ok_or(PlayError::Untimed)?;
        Ok(pass.now + self.lead(pass.delivery).unwrap_or(pass.deck_delivery))
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

    /// The frame a segment cued for `bound` is ready to start on.
    pub(super) fn prepared(&self, bound: SessionFrame) -> Result<SessionFrame, PlayError> {
        let rate = f64::from(self.host.sample_rate().get());
        let frames = (self.config.preparation().as_secs_f64() * rate)
            .ceil()
            .to_usize()
            .ok_or(PlayError::Untimed)?;
        Ok(bound + FrameCount::new(frames))
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
            is_final(grid) && extent.is_some(),
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

use kithara_beat::BeatGridModel;
use kithara_play::{Outbox, PlayError, Position, Track, TrackSnapshot, TrackStatus};
use kithara_signal::{FrameCount, SessionFrame};
use kithara_sync::{Coverage, SourceGrid, SpeedCurve, boundary, speed_curve};
use num_traits::ToPrimitive;

use super::{Linked, LinkedPlayer, state::Cue};
use crate::covers;

pub(super) fn cue_position(track: &TrackSnapshot) -> Position {
    if matches!(track.status, TrackStatus::Ended { .. }) {
        Position::ZERO
    } else {
        track.position
    }
}

/// Where a grid places a transport command.
pub(super) enum Placement {
    /// The command lands on this position.
    At(Position),
    /// A later grid may place it.
    Wait,
    /// The final grid ends before it.
    Refused,
}

/// What a final grid does with a command past its last beat.
#[derive(Clone, Copy)]
pub(super) enum PastEnd {
    /// The command lands on the last beat.
    LastBeat,
    /// The command is refused.
    Refused,
}

/// Places `cue` by its own rule; past that, a final grid answers by
/// `past_end` and a provisional one leaves the command waiting.
pub(super) fn place(grid: &BeatGridModel, cue: Cue, past_end: PastEnd) -> Placement {
    let placed = match cue {
        Cue::Covered(position) if covers(grid.geometry(), position) => Some(position),
        Cue::Covered(position) | Cue::Beat(position) => boundary(grid.geometry(), position),
    };
    if let Some(position) = placed {
        return Placement::At(position);
    }

    if !is_final(grid) {
        return Placement::Wait;
    }

    match past_end {
        PastEnd::LastBeat => grid
            .geometry()
            .beats()
            .last()
            .and_then(|beat| Position::try_from_secs_f64(beat.at).ok())
            .map_or(Placement::Refused, Placement::At),
        PastEnd::Refused => Placement::Refused,
    }
}

/// A final grid is the last answer for its track, so what it leaves out stays out.
fn is_final(grid: &BeatGridModel) -> bool {
    matches!(grid.as_raw().state, kithara_beat::BeatGridState::Final)
}

impl<P> Linked<P> {
    /// The earliest frame a batch lands on: a sounding deck's lane lead, else
    /// the deck delivery of a silent start.
    pub(super) fn earliest<S>(&self, out: &Outbox<'_, S>) -> Result<SessionFrame, PlayError>
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

    /// The configured plan horizon in Host frames, saturating at the largest
    /// frame count.
    pub(super) fn horizon(&self) -> u64 {
        let frames = self.config.horizon().as_nanos() * u128::from(self.host.sample_rate().get())
            / 1_000_000_000;
        u64::try_from(frames).unwrap_or(u64::MAX)
    }

    /// Records how far the speed plan sent from `start` reaches.
    pub(super) fn plan_sent(&mut self, start: SessionFrame, coverage: Coverage) {
        self.plan_end = match coverage {
            Coverage::Until(frames) => usize::try_from(frames)
                .ok()
                .map(|frames| start + FrameCount::new(frames)),
            Coverage::GridEnd => None,
        };
    }

    pub(super) fn curve(
        &self,
        grid: &SourceGrid,
        position: Position,
        at: SessionFrame,
        landing: Option<(Position, SessionFrame)>,
    ) -> Result<(SpeedCurve, Coverage), PlayError> {
        speed_curve(&self.host, grid, position, at, landing, self.horizon()).ok_or_else(|| {
            let rate = f64::from(self.host.sample_rate().get());
            let track = grid.bpm_at_frame(position.as_secs_f64() * rate);
            let host = self.host.anchor_at(at).tempo_at(at) * 60.0;
            PlayError::Internal(format!(
                "no host speed law: track at {track:?} BPM, host at {host} BPM, position {position:?}"
            ))
        })
    }
}

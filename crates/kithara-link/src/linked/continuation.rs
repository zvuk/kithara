use kithara_command::Seq;
use kithara_play::{Outbox, PlayError, Position, Settled, Track, TrackStatus};
use kithara_render::{LaneFrame, bridge::SlotMark};
use kithara_signal::{FrameCount, SessionFrame};
use kithara_sync::{Slew, SlewCause, slew};

use super::{
    Linked, LinkedPlayer,
    state::{Cause, ContinuationAdmission, RunningCorrection, SourceContinuation},
};
use crate::covers;

impl<P: Track> Linked<P> {
    pub(super) fn prepare_synced_continuation(
        &mut self,
        at: SessionFrame,
        position: Position,
        speed: f64,
        out: &mut Outbox<'_>,
    ) -> Result<bool, PlayError> {
        if !self
            .inner
            .prepare_source_continuation(at, position, speed, out)?
        {
            return Ok(false);
        }
        if let Some(entry) = &self.continuation {
            return Ok(matches!(entry.admission, ContinuationAdmission::Preparing));
        }
        let Some(grid) = self
            .grid
            .as_ref()
            .filter(|grid| covers(grid.geometry(), position))
        else {
            return Ok(false);
        };
        let source = self.source(grid)?;
        let Some(Slew::Corrects {
            curve,
            frames,
            coverage,
        }) = slew(
            &self.host,
            &source,
            position,
            at,
            self.config.epsilon(),
            SlewCause::Drift { from: speed },
            self.horizon(),
        )
        else {
            return Err(PlayError::NotReady);
        };
        let frames = usize::try_from(frames)
            .map(FrameCount::new)
            .map_err(|error| PlayError::Internal(error.to_string()))?;
        if let Some(cue) = self.inner.cue(position, curve, out)? {
            self.continuation = Some(SourceContinuation {
                cue,
                frames,
                coverage,
                admission: ContinuationAdmission::Preparing,
            });
            self.cue = None;
            self.waiting = None;
            self.alignment = None;
            self.plan_end = None;
        }
        Ok(false)
    }

    pub(super) fn bind_continuation(&mut self, seq: Option<Seq>) {
        if let Some(entry) = &mut self.continuation
            && let ContinuationAdmission::Staged(to) = entry.admission
        {
            entry.admission = seq.map_or(ContinuationAdmission::Preparing, |seq| {
                ContinuationAdmission::Submitted { to, seq }
            });
        }
    }

    pub(super) fn settle_continuation(
        &mut self,
        entry: &SourceContinuation,
        settled: &Settled,
        cut: Option<(SlotMark, Position)>,
    ) {
        let ContinuationAdmission::Submitted { to, seq: submitted } = entry.admission else {
            return;
        };
        let (Settled::Applied { seq, at }, Some((mark, media_origin))) = (settled, cut) else {
            return;
        };
        let track = self.inner.snapshot();
        let track = track.as_ref();
        if *seq != submitted
            || !self.synced()
            || track.slot != Some(to)
            || !matches!(track.status, TrackStatus::Playing { since } if since == *at)
            || mark.session != *at
            || mark.position.checked_sub(media_origin).is_none()
            || track.mark.is_some_and(|current| {
                current.session >= mark.session && current.lane.segment != mark.lane.segment
            })
        {
            return;
        }
        let Some(origin) = mark.session_at(LaneFrame {
            frame: 0,
            ..mark.lane
        }) else {
            return;
        };
        self.correction = Some((
            entry.cue,
            RunningCorrection {
                at: origin,
                frames: entry.frames,
                segment: mark.lane.segment,
            },
        ));
        self.plan_sent(origin, entry.coverage);
        self.alignment = Some(Cause::Drift);
    }
}

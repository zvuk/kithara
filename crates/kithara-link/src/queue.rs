use kithara_bufpool::HasPool;
use kithara_command::Seq;
use kithara_play::{Outbox, PlayError, TrackFactory};
use kithara_queue::Queue;
use kithara_signal::{FrameCount, SessionFrame};

use crate::{GridAnswer, LinkedFactory, LinkedPlayer, TempoTrajectory};

impl<S, F> LinkedPlayer<S> for Queue<S, LinkedFactory<F>>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
    F: TrackFactory<S>,
{
    fn sync(&mut self, on: bool, out: &mut Outbox<'_, S>) -> Result<Option<Seq>, PlayError> {
        let sent = sync_tracks(self.tracks_mut(), on, out)?;
        self.factory_mut().set_synced(on);
        Ok(sent)
    }

    fn retime(
        &mut self,
        trajectory: &TempoTrajectory,
        frames: &[SessionFrame],
        out: &mut Outbox<'_, S>,
    ) {
        self.factory_mut().set_trajectory(trajectory);
        for track in self.tracks_mut() {
            track.retime(trajectory, frames, out);
        }
    }

    fn grid(&mut self, answer: GridAnswer, out: &mut Outbox<'_, S>) {
        for track in self.tracks_mut() {
            track.grid(answer.clone(), out);
        }
    }

    delegate::delegate! {
        to self.factory() {
            fn synced(&self) -> bool;
        }
    }

    fn lead(&self, delivery: FrameCount) -> Option<FrameCount> {
        self.tracks_active()
            .filter_map(|track| track.lead(delivery))
            .max()
    }

    fn lane_room(&self) -> usize {
        self.tracks_active()
            .filter(|track| track.synced())
            .map(LinkedPlayer::lane_room)
            .min()
            .unwrap_or(usize::MAX)
    }

    fn scope_parts(&self) -> usize {
        self.tracks_active().map(LinkedPlayer::scope_parts).sum()
    }

    fn retime_applied(&mut self, at: SessionFrame) -> Option<bool> {
        let mut awaiting = false;
        for track in self.tracks_mut() {
            match track.retime_applied(at) {
                Some(true) => return Some(true),
                None => awaiting = true,
                Some(false) => {}
            }
        }
        (!awaiting).then_some(false)
    }

    fn realign(&mut self, trajectory: &TempoTrajectory, at: SessionFrame, out: &mut Outbox<'_, S>) {
        self.factory_mut().set_trajectory(trajectory);
        for track in self.tracks_mut() {
            track.realign(trajectory, at, out);
        }
    }
}

/// Syncs each track in turn; a refusal turns back the tracks this call
/// turned and answers the refusal.
fn sync_tracks<'a, S, T>(
    mut tracks: impl Iterator<Item = &'a mut T>,
    on: bool,
    out: &mut Outbox<'_, S>,
) -> Result<Option<Seq>, PlayError>
where
    T: LinkedPlayer<S> + 'a,
{
    let Some(track) = tracks.next() else {
        return Ok(None);
    };
    let before = track.synced();
    let sent = track.sync(on, out)?;

    match sync_tracks(tracks, on, out) {
        Ok(later) => Ok(later.or(sent)),
        Err(error) => {
            if track.synced() != before {
                track.sync(!on, out)?;
            }
            Err(error)
        }
    }
}

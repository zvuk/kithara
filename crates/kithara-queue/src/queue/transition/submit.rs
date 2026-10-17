use kithara_bufpool::HasPool;
use kithara_command::{Seq, When};
use kithara_play::{
    CrossfadeSettings, FadeDir, Outbox, PlayError, Player, Slot, Track, TrackCommand, TrackFactory,
    TrackStatus as PlayingStatus,
};
use kithara_signal::SessionFrame;

use super::super::{
    Queue,
    types::{PendingHandover, Target},
};

impl<S, F> Queue<S, F>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
    F: TrackFactory<S>,
{
    pub(in crate::queue) fn send_transition(
        &mut self,
        incoming: usize,
        frame: SessionFrame,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, PlayError> {
        if self.pending_handover.is_some() {
            return Ok(None);
        }
        let target = self.target.ok_or(PlayError::NotReady)?;
        if !self.prepare_transition(incoming, frame, out)? {
            return Ok(None);
        }
        let slot = self
            .active
            .get(incoming)
            .ok_or(PlayError::NoActiveSlot)?
            .slot;
        let replacement = self.active.is_replacement(incoming);
        let current = self.active_current_index();
        let outgoing = current.filter(|index| {
            self.active.get(*index).is_some_and(|current| {
                current.slot != slot
                    && !matches!(
                        current.track.snapshot().as_ref().status,
                        PlayingStatus::Failed { .. } | PlayingStatus::Released
                    )
            })
        });
        let predecessor = self.chain_predecessor(target, replacement);
        let chain = predecessor.is_some();
        let at = self.transition_when(incoming, frame)?;
        let sent = if chain {
            self.active
                .get_mut(incoming)
                .ok_or(PlayError::NoActiveSlot)?
                .track
                .apply(
                    TrackCommand::PlayAfter {
                        track: predecessor.ok_or(PlayError::NoActiveSlot)?,
                    },
                    out,
                )?
        } else {
            self.fade_transition(incoming, outgoing, at, target.settings, out)?
        };
        if let Some(seq) = sent {
            self.pending_handover = Some(PendingHandover {
                seq,
                item: target.to,
                slot,
                replacement,
                reason: target.reason,
            });
            if let Some(target) = &mut self.target {
                target.chained = chain;
            }
        }
        Ok(sent)
    }
    fn prepare_transition(
        &mut self,
        incoming: usize,
        frame: SessionFrame,
        out: &mut Outbox<'_, S>,
    ) -> Result<bool, PlayError> {
        if out.deck_available() == 0 {
            return Err(PlayError::Full("deck"));
        }
        if !self
            .active
            .get_mut(incoming)
            .ok_or(PlayError::NoActiveSlot)?
            .track
            .prepare_entry(frame, out)?
        {
            return Ok(false);
        }
        Ok(true)
    }

    fn chain_predecessor(&self, target: Target, replacement: bool) -> Option<Slot> {
        let predecessor = self
            .active_current_index()
            .and_then(|index| self.active.get(index))
            .filter(|active| {
                matches!(
                    active.track.snapshot().as_ref().status,
                    PlayingStatus::Playing { .. }
                )
            })
            .map(|active| active.slot);
        let chain = target.auto
            && (self.config.settings.gapless() || target.settings.duration == 0.0)
            && predecessor.is_some()
            && !replacement;
        if chain { predecessor } else { None }
    }

    fn transition_when(
        &self,
        incoming: usize,
        frame: SessionFrame,
    ) -> Result<When<SessionFrame>, PlayError> {
        Ok(
            if self.initial_attachment(incoming)? && frame == self.earliest()? {
                When::Next
            } else {
                When::At(frame)
            },
        )
    }

    fn fade_transition(
        &mut self,
        incoming: usize,
        outgoing: Option<usize>,
        at: When<SessionFrame>,
        settings: CrossfadeSettings,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, PlayError> {
        let slot = self
            .active
            .get(incoming)
            .ok_or(PlayError::NoActiveSlot)?
            .slot;
        let replacement = self.active.is_replacement(incoming);
        let result = out.together_owned(at, |out| {
            let active = self
                .active
                .get_mut(incoming)
                .ok_or(PlayError::NoActiveSlot)?;
            if replacement {
                active.track.apply(TrackCommand::Seat { slot, at }, out)?;
            }
            active.track.apply(
                TrackCommand::Fade {
                    at,
                    settings,
                    dir: FadeDir::In,
                },
                out,
            )?;
            if let Some(index) = outgoing {
                self.active
                    .get_mut(index)
                    .ok_or(PlayError::NoActiveSlot)?
                    .track
                    .apply(
                        TrackCommand::Fade {
                            at,
                            settings,
                            dir: FadeDir::Out,
                        },
                        out,
                    )?;
            }
            Ok(())
        });
        let track = &mut self
            .active
            .get_mut(incoming)
            .ok_or(PlayError::NoActiveSlot)?
            .track;
        match result {
            Ok(((), Some(seq))) => {
                track.finish_group(Ok(seq));
                if let Some(index) = outgoing {
                    self.active
                        .get_mut(index)
                        .ok_or(PlayError::NoActiveSlot)?
                        .track
                        .finish_group(Ok(seq));
                }
                Ok(Some(seq))
            }
            Ok(((), None)) => Ok(None),
            Err((error, mut parts)) => {
                track.finish_group(Err(&mut parts));
                if let Some(index) = outgoing {
                    self.active
                        .get_mut(index)
                        .ok_or(PlayError::NoActiveSlot)?
                        .track
                        .finish_group(Err(&mut parts));
                }
                Err(error)
            }
        }
    }
}

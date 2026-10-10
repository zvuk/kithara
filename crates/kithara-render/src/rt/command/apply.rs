use kithara_command::{Due, LevelInbox, Seq, Target};
use kithara_config::LiveConfig;
use kithara_signal::{SegmentId, SessionFrame};

use super::{
    super::{
        processor::Deck,
        track::{PlayerResource, PlayerTrack},
    },
    state::*,
};
use crate::bridge::{DeckEqChange, DeckPart, DeckProtocol, Fade, FadeDir, Returned, Slot};

impl Deck {
    pub(in crate::rt) fn apply(
        &mut self,
        part: DeckPart,
        at: SessionFrame,
        seq: Seq,
        context_valid: bool,
    ) -> Option<DeckPart> {
        let part = match part {
            DeckPart::Fade {
                slot,
                settings,
                dir: FadeDir::Out,
            } => DeckPart::Stop {
                slot,
                fade: Fade::Crossfade(settings),
            },
            part => part,
        };
        if let Some(slot) = shifted_slot(&part) {
            self.ended[slot.index()] = false;
        }
        match part {
            DeckPart::Attach { slot, pcm, segment } => {
                let track = self.track(pcm, segment);
                self.tracks
                    .put(slot, track)
                    .err()
                    .map(|resource| returned_pcm(slot, resource))
            }
            DeckPart::Detach { slot } => {
                let mut track = self.tracks.take(slot)?;
                if context_valid {
                    self.tails[slot.index()].fill(
                        &mut track,
                        self.declick_frames,
                        &self.metrics,
                        &mut self.recycle[slot.index()],
                    );
                }
                Some(returned_pcm(slot, track.into_resource()))
            }
            DeckPart::Start { slot, fade } => {
                if let Some(track) = self.tracks.at_mut(slot) {
                    track.start(fade);
                }
                Some(DeckPart::Start { slot, fade })
            }
            DeckPart::Stop { slot, fade } => {
                let track = self.tracks.at_mut(slot)?;
                track.stop(fade, at);
                if let Some(resume) = track.stop_resume() {
                    track.clear_stop();
                    Some(DeckPart::Returned(Returned::Stopped { slot, resume }))
                } else {
                    self.stops[slot.index()] = Some(seq);
                    Some(DeckPart::Stop { slot, fade })
                }
            }
            DeckPart::Adopt { slot, segment } => {
                if let Some(track) = self.tracks.at_mut(slot) {
                    if context_valid {
                        self.tails[slot.index()].fill(
                            track,
                            self.declick_frames,
                            &self.metrics,
                            &mut self.recycle[slot.index()],
                        );
                    }
                    track.adopt(segment);
                    track.recycle_obsolete(&mut self.recycle[slot.index()]);
                }
                Some(DeckPart::Adopt { slot, segment })
            }
            DeckPart::Fade {
                slot,
                settings,
                dir,
            } => {
                if let Some(track) = self.tracks.at_mut(slot) {
                    track.fade(settings, dir);
                }
                Some(DeckPart::Fade {
                    slot,
                    settings,
                    dir,
                })
            }
            DeckPart::Replace { slot, pcm, segment } => {
                let mut track = self.track(pcm, segment);
                track.start(Fade::Declick);
                track.snap_gate();
                match self.tracks.replace(slot, track) {
                    Ok(mut old) => {
                        if old.state() == crate::bridge::SlotState::Playing {
                            self.metrics.record_evicted_playing();
                        }
                        if context_valid {
                            self.tails[slot.index()].fill(
                                &mut old,
                                self.evict_frames,
                                &self.metrics,
                                &mut self.recycle[slot.index()],
                            );
                        }
                        Some(returned_pcm(slot, old.into_resource()))
                    }
                    Err(resource) => Some(returned_pcm(slot, resource)),
                }
            }
            DeckPart::Chain { from, to } => {
                self.ended[to.index()] = false;
                if let Some(track) = self.tracks.at_mut(to) {
                    track.start(Fade::Crossfade(crate::CrossfadeSettings {
                        duration: 0.0,
                        ..Default::default()
                    }));
                    track.snap_gate();
                }
                Some(DeckPart::Chain { from, to })
            }
            DeckPart::Mix(change) => {
                self.mix.apply_change(change);
                self.render.set_gain(self.mix.gain());
                Some(DeckPart::Mix(change))
            }
            DeckPart::Eq(DeckEqChange::Gain { band, gain }) => {
                self.render.set_eq_gain(band, gain);
                Some(DeckPart::Eq(DeckEqChange::Gain { band, gain }))
            }
            DeckPart::Eq(DeckEqChange::Layout(layout)) => self
                .render
                .take_eq_layout(layout)
                .map(|layout| DeckPart::Returned(Returned::Eq(layout))),
            DeckPart::Returned(returned) => Some(DeckPart::Returned(returned)),
        }
    }

    pub(in crate::rt) fn track(&self, pcm: Box<PlayerResource>, segment: SegmentId) -> PlayerTrack {
        PlayerTrack::builder()
            .sample_rate(self.sample_rate)
            .declick(self.declick)
            .segment(segment)
            .build(pcm)
    }

    pub(in crate::rt) fn interrupt_stop(
        &mut self,
        due: &mut Due<'_, DeckProtocol>,
        slot: Slot,
        remaining: usize,
    ) {
        let Some(seq) = self.stops[slot.index()].take() else {
            return;
        };
        let resume = self
            .tracks
            .at_mut(slot)
            .and_then(|track| track.interrupt_stop(due.at()));
        let Some(resume) = resume else { return };
        if seq == due.seq() {
            replace_stop(&mut due.commands_mut()[remaining..], slot, resume);
        } else {
            self.interrupted[slot.index()] = Some((seq, resume));
        }
    }

    pub(in crate::rt) fn finish_stops(&mut self, level: &mut LevelInbox<'_, DeckProtocol>) {
        for (index, interrupted) in self.interrupted.iter_mut().enumerate() {
            let Some((seq, resume)) = interrupted.take() else {
                continue;
            };
            let slot = Slot::new(u16::try_from(index).unwrap_or(u16::MAX));
            if let Some(commands) = level.committed_mut(seq) {
                replace_stop(commands, slot, resume);
                if stops_complete(commands) {
                    level.complete(seq, ());
                }
            }
        }
        for index in 0..self.stops.len() {
            let Some(seq) = self.stops[index] else {
                continue;
            };
            let Some(commands) = level.committed_mut(seq) else {
                self.clear_stops(seq);
                continue;
            };
            let slot = Slot::new(u16::try_from(index).unwrap_or(u16::MAX));
            let Some(resume) = self.tracks.at(slot).and_then(PlayerTrack::stop_resume) else {
                continue;
            };
            replace_stop(commands, slot, resume);
            self.stops[index] = None;
            if let Some(track) = self.tracks.at_mut(slot) {
                track.clear_stop();
            }
            if stops_complete(commands) {
                level.complete(seq, ());
            }
        }
    }

    pub(in crate::rt) fn clear_stops(&mut self, seq: Seq) {
        for (index, pending) in self.stops.iter_mut().enumerate() {
            if *pending != Some(seq) {
                continue;
            }
            *pending = None;
            let slot = Slot::new(u16::try_from(index).unwrap_or(u16::MAX));
            if let Some(track) = self.tracks.at_mut(slot) {
                track.clear_stop();
            }
        }
    }
}

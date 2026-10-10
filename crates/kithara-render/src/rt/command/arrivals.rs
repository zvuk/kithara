use kithara_command::{Due, LevelInbox, Target};
use kithara_signal::SessionFrame;

use super::{
    super::{processor::Deck, track::PlayerTrack},
    state::*,
};
use crate::bridge::{DeckPart, DeckProtocol, DeckRefusal, Slot};

impl Deck {
    pub(in crate::rt) fn arrivals(&mut self, level: &mut LevelInbox<'_, DeckProtocol>) {
        self.release_armed(level);
        while let Some(deferred) = level.next_deferred() {
            let (from, action) = match deferred.commands() {
                [DeckPart::Chain { from, to }] => (*from, EndAction::Chain { to: *to }),
                [DeckPart::Adopt { slot, segment }] => {
                    (*slot, EndAction::Adopt { segment: *segment })
                }
                _ => {
                    deferred.refuse(DeckRefusal::Deferral);
                    continue;
                }
            };
            if let Err(reason) = self.validate(deferred.commands()) {
                deferred.refuse(reason);
                continue;
            }
            if self.armed[from.index()].is_some() {
                deferred.refuse(DeckRefusal::Occupied { slot: from });
                continue;
            }
            if let Some(seq) = deferred.park() {
                self.armed[from.index()] = Some(Armed { seq, action });
            }
        }
    }

    pub(in crate::rt) fn take_due(&mut self, mut due: Due<'_, DeckProtocol>, context_valid: bool) {
        if due
            .commands()
            .iter()
            .any(|part| matches!(part, DeckPart::Chain { .. }))
        {
            due.refuse(DeckRefusal::Deferral);
            return;
        }
        if let Err(reason) = self.validate(due.commands()) {
            due.refuse(reason);
            return;
        }
        let at = due.at();
        let seq = due.seq();
        let length = due.commands().len();
        for remaining in (0..length).rev() {
            let part = due.commands_mut().remove(0);
            if let Some(slot) = shifted_slot(&part) {
                self.interrupt_stop(&mut due, slot, remaining);
            }
            if let Some(returned) = self.apply(part, at, seq, context_valid) {
                due.commands_mut().push(returned);
            }
        }
        if due
            .commands()
            .iter()
            .any(|part| matches!(part, DeckPart::Stop { .. }))
        {
            due.commit();
        } else {
            due.apply(());
        }
    }

    pub(in crate::rt) fn resolve_armed(
        &mut self,
        level: &mut LevelInbox<'_, DeckProtocol>,
        start: SessionFrame,
        at: SessionFrame,
    ) {
        self.release_armed(level);
        for index in 0..self.armed.len() {
            let Some(armed) = self.armed[index] else {
                continue;
            };
            let from = Slot::new(u16::try_from(index).unwrap_or(u16::MAX));
            let refusal = if self.tracks.at(from).is_none() {
                Some(DeckRefusal::Empty { slot: from })
            } else {
                match armed.action {
                    EndAction::Chain { to } if self.tracks.at(to).is_none() => {
                        Some(DeckRefusal::Empty { slot: to })
                    }
                    EndAction::Adopt { segment }
                        if self
                            .tracks
                            .at(from)
                            .is_some_and(|track| segment <= track.segment()) =>
                    {
                        Some(DeckRefusal::Outdated { slot: from })
                    }
                    _ => None,
                }
            };
            if let Some(refusal) = refusal {
                self.armed[index] = None;
                if let Some(due) = level.resume(armed.seq, start, at) {
                    due.refuse(refusal);
                }
            }
        }
    }

    pub(in crate::rt) fn fire_ended(
        &mut self,
        level: &mut LevelInbox<'_, DeckProtocol>,
        start: SessionFrame,
        at: SessionFrame,
        context_valid: bool,
    ) {
        loop {
            let next =
                self.armed
                    .iter()
                    .enumerate()
                    .filter_map(|(index, armed)| {
                        let armed = (*armed)?;
                        let from = Slot::new(u16::try_from(index).ok()?);
                        (self.ended[index]
                            || self.tracks.at(from).is_some_and(|track| {
                                track.state() == crate::bridge::SlotState::Ended
                            }))
                        .then_some((index, armed))
                    })
                    .min_by_key(|(_, armed)| armed.seq);
            let Some((index, armed)) = next else { break };
            self.armed[index] = None;
            let Some(mut due) = level.resume(armed.seq, start, at) else {
                continue;
            };
            if let Err(refusal) = self.validate(due.commands()) {
                due.refuse(refusal);
                continue;
            }
            let seq = due.seq();
            let part = due.commands_mut().remove(0);
            match &part {
                DeckPart::Chain { from, to } => {
                    self.interrupt_stop(&mut due, *from, 0);
                    if to != from {
                        self.interrupt_stop(&mut due, *to, 0);
                    }
                }
                _ => {
                    if let Some(slot) = shifted_slot(&part) {
                        self.interrupt_stop(&mut due, slot, 0);
                    }
                }
            }
            if let Some(returned) = self.apply(part, at, seq, context_valid) {
                due.commands_mut().push(returned);
            }
            due.apply(());
            self.finish_stops(level);
            self.resolve_armed(level, start, at);
        }
    }

    pub(in crate::rt) fn release_armed(&mut self, level: &LevelInbox<'_, DeckProtocol>) {
        for entry in &mut self.armed {
            if entry.is_some_and(|armed| !level.is_parked(armed.seq)) {
                *entry = None;
            }
        }
    }

    pub(in crate::rt) fn validate(&mut self, commands: &[DeckPart]) -> Result<(), DeckRefusal> {
        for (entry, slot) in self.held.iter_mut().zip(self.tracks.slots()) {
            *entry = self.tracks.at(slot).map(PlayerTrack::segment);
        }
        for part in commands {
            match part {
                DeckPart::Attach { slot, segment, .. } => {
                    let Some(entry) = self.held.get_mut(slot.index()) else {
                        return Err(DeckRefusal::Empty { slot: *slot });
                    };
                    if entry.is_some() {
                        return Err(DeckRefusal::Occupied { slot: *slot });
                    }
                    *entry = Some(*segment);
                }
                DeckPart::Detach { slot } => {
                    let Some(entry) = self
                        .held
                        .get_mut(slot.index())
                        .filter(|entry| entry.is_some())
                    else {
                        return Err(DeckRefusal::Empty { slot: *slot });
                    };
                    *entry = None;
                }
                DeckPart::Adopt { slot, segment } => {
                    let Some(entry) = self
                        .held
                        .get_mut(slot.index())
                        .filter(|entry| entry.is_some())
                    else {
                        return Err(DeckRefusal::Empty { slot: *slot });
                    };
                    if entry.is_some_and(|current| *segment <= current) {
                        return Err(DeckRefusal::Outdated { slot: *slot });
                    }
                    *entry = Some(*segment);
                }
                DeckPart::Replace { slot, segment, .. } => {
                    let Some(entry) = self
                        .held
                        .get_mut(slot.index())
                        .filter(|entry| entry.is_some())
                    else {
                        return Err(DeckRefusal::Empty { slot: *slot });
                    };
                    *entry = Some(*segment);
                }
                DeckPart::Start { slot, .. }
                | DeckPart::Stop { slot, .. }
                | DeckPart::Fade { slot, .. } => {
                    if !self.held.get(slot.index()).is_some_and(Option::is_some) {
                        return Err(DeckRefusal::Empty { slot: *slot });
                    }
                }
                DeckPart::Chain { from, to } => {
                    for slot in [*from, *to] {
                        if !self.held.get(slot.index()).is_some_and(Option::is_some) {
                            return Err(DeckRefusal::Empty { slot });
                        }
                    }
                }
                DeckPart::Mix(_) | DeckPart::Eq(_) | DeckPart::Returned(_) => {}
            }
        }
        Ok(())
    }
}

use kithara_command::{Due, LevelInbox, Seq};
use kithara_signal::{SessionEpoch, SessionFrame};

use super::super::{processor::Deck, track::PlayerTrack};
use crate::bridge::{DeckPart, DeckProtocol, DeckRefusal, ParameterCurve, Slot, SlotState};

fn gain_curve(commands: &[DeckPart], slot: Slot) -> Option<&ParameterCurve> {
    commands.iter().find_map(|part| match part {
        DeckPart::GainProgram {
            slot: target,
            program,
            ..
        } if *target == slot => Some(program.curve()),
        _ => None,
    })
}

pub(in crate::rt) fn output_curve<'block>(
    level: &'block mut LevelInbox<'_, DeckProtocol>,
    seq: Option<Seq>,
    slot: Slot,
) -> Option<&'block ParameterCurve> {
    let commands = level
        .committed_mut(seq?)
        .expect("active output owns its committed batch");
    Some(gain_curve(commands, slot).expect("active output owns its slot program"))
}

fn invalidates_output(part: &DeckPart, slot: Slot) -> bool {
    matches!(part,
        DeckPart::Stop { slot: target, .. }
        | DeckPart::Adopt { slot: target, .. }
        | DeckPart::Detach { slot: target }
        | DeckPart::Replace { slot: target, .. }
        | DeckPart::Attach { slot: target, .. } if *target == slot)
}

impl Deck {
    pub(in crate::rt) fn validate_output_programs(
        &self,
        commands: &[DeckPart],
    ) -> Result<(), DeckRefusal> {
        for (index, part) in commands.iter().enumerate() {
            let DeckPart::GainProgram {
                slot,
                binding,
                authority,
                ..
            } = part
            else {
                continue;
            };
            let track = self
                .tracks
                .at(*slot)
                .ok_or(DeckRefusal::Empty { slot: *slot })?;
            if self.epoch != Some(binding.epoch()) {
                return Err(DeckRefusal::AxisRestarted);
            }
            if track.slot_binding(binding.epoch()) != Some(*binding)
                || track.output_snapshot().authority() != *authority
                || track.state() == SlotState::Ended
                || commands.iter().any(|part| invalidates_output(part, *slot))
            {
                return Err(DeckRefusal::Outdated { slot: *slot });
            }
            if commands[..index].iter().any(|part| {
                matches!(part,
                DeckPart::GainProgram { slot: target, .. } if *target == *slot)
            }) {
                return Err(DeckRefusal::Occupied { slot: *slot });
            }
        }
        Ok(())
    }

    pub(in crate::rt) fn output_finished(&self, commands: &[DeckPart], seq: Seq) -> bool {
        !self
            .tracks
            .slots()
            .any(|slot| self.tracks.at(slot).and_then(PlayerTrack::output_program) == Some(seq))
            && !commands
                .iter()
                .any(|part| matches!(part, DeckPart::Stop { .. }))
    }

    pub(in crate::rt) fn complete_output(
        &self,
        level: &mut LevelInbox<'_, DeckProtocol>,
        seq: Seq,
    ) {
        if level
            .committed_mut(seq)
            .is_some_and(|commands| self.output_finished(commands, seq))
        {
            level.complete(seq, ());
        }
    }

    pub(in crate::rt) fn interrupt_output(
        &mut self,
        due: &mut Due<'_, DeckProtocol>,
        part: &DeckPart,
    ) {
        let slot = match part {
            DeckPart::GainProgram { slot, .. }
            | DeckPart::Stop { slot, .. }
            | DeckPart::Adopt { slot, .. }
            | DeckPart::Detach { slot }
            | DeckPart::Replace { slot, .. } => *slot,
            _ => return,
        };
        let Some(track) = self.tracks.at_mut(slot) else {
            return;
        };
        let at = due.at();
        let current = due.seq();
        let previous = track.output_program();
        let curve = match previous {
            Some(seq) if seq == current => gain_curve(due.commands(), slot),
            Some(seq) => Some(
                gain_curve(
                    due.committed_mut(seq)
                        .expect("active output owns its committed batch"),
                    slot,
                )
                .expect("active output owns its slot program"),
            ),
            None => None,
        };
        let previous = if matches!(part, DeckPart::GainProgram { .. }) {
            track.cancel_output(curve, at)
        } else {
            track.revoke_output(current, curve, at)
        };
        if let Some(seq) = previous
            && due
                .committed_mut(seq)
                .is_some_and(|commands| self.output_finished(commands, seq))
        {
            due.complete(seq, ());
        }
    }

    pub(in crate::rt) fn observe_output_epoch(
        &mut self,
        level: &mut LevelInbox<'_, DeckProtocol>,
        epoch: SessionEpoch,
        at: SessionFrame,
    ) {
        if self.epoch == Some(epoch) {
            return;
        }
        for slot in self.tracks.slots() {
            let previous = self
                .tracks
                .at_mut(slot)
                .and_then(|track| track.cancel_output(None, at));
            if let Some(seq) = previous {
                self.complete_output(level, seq);
            }
        }
        self.epoch = Some(epoch);
    }
}

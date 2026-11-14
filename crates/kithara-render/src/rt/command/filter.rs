use kithara_command::{Due, LevelInbox, ScopeId, Seq};
use kithara_signal::{SessionEpoch, SessionFrame};

use super::{
    super::processor::Deck,
    parameter::{ParameterState, ProgramState},
};
use crate::bridge::{
    DeckMixSettingsChange, DeckPart, DeckProtocol, DeckRefusal, EqGainProgram, FilterBinding,
    FilterProgram, FilterSnapshot,
};

/// Program metadata of the deck filter knob; the inbox retains the program.
pub(in crate::rt) struct FilterPrograms {
    scope: ScopeId,
    knob: ParameterState,
}

impl FilterPrograms {
    pub(in crate::rt) fn new(scope: ScopeId) -> Self {
        Self {
            scope,
            knob: ParameterState::default(),
        }
    }

    pub(in crate::rt) fn has_seq(&self, seq: Seq) -> bool {
        self.knob.program.active() == Some(seq)
    }

    pub(in crate::rt) fn publish(
        &self,
        snapshot: &mut FilterSnapshot,
        epoch: Option<SessionEpoch>,
        knob: f64,
    ) {
        snapshot.publish(
            epoch.map(|epoch| FilterBinding::new(self.scope, None, epoch)),
            knob,
            self.knob.program.applied(),
            self.knob.authority,
        );
    }

    pub(in crate::rt) fn program<'a>(
        &self,
        level: &'a LevelInbox<'_, DeckProtocol>,
    ) -> Option<&'a FilterProgram> {
        self.knob.program.active().map(|seq| {
            program_in(level.committed(seq).expect("active filter owns its batch"))
                .expect("active filter owns its program")
        })
    }

    /// Whether the knob program runs after `commands` apply.
    fn active_after(&self, commands: &[DeckPart]) -> bool {
        commands
            .iter()
            .any(|part| matches!(part, DeckPart::FilterProgram { .. }))
            || (self.knob.program.active().is_some() && !commands.iter().any(cancels))
    }
}

fn program_in(commands: &[DeckPart]) -> Option<&FilterProgram> {
    commands.iter().find_map(|part| match part {
        DeckPart::FilterProgram { program, .. } => Some(program),
        _ => None,
    })
}

fn cancels(part: &DeckPart) -> bool {
    matches!(
        part,
        DeckPart::Mix(DeckMixSettingsChange::Filter(_)) | DeckPart::FilterProgram { .. }
    )
}

impl Deck {
    /// Active filter programs after `commands` apply.
    pub(in crate::rt) fn filter_programs_after(&self, commands: &[DeckPart]) -> usize {
        usize::from(self.filter_programs.active_after(commands))
    }

    pub(in crate::rt) fn validate_filter_programs(
        &self,
        commands: &[DeckPart],
    ) -> Result<(), DeckRefusal> {
        let mut programs = commands.iter().filter_map(|part| match part {
            DeckPart::FilterProgram {
                binding, authority, ..
            } => Some((binding, authority)),
            _ => None,
        });
        let Some((binding, authority)) = programs.next() else {
            return Ok(());
        };
        if self.epoch != Some(binding.epoch()) {
            return Err(DeckRefusal::AxisRestarted);
        }
        if programs.next().is_some()
            || binding.scope() != self.filter_programs.scope
            || binding.instance().is_some()
            || *authority != self.filter_programs.knob.authority
            || commands
                .iter()
                .any(|part| matches!(part, DeckPart::Mix(DeckMixSettingsChange::Filter(_))))
        {
            return Err(DeckRefusal::FilterOutdated);
        }
        if self.eq_programs_after(commands) + 1 > EqGainProgram::MAX_ACTIVE {
            return Err(DeckRefusal::FilterProgramCapacity);
        }
        Ok(())
    }

    pub(in crate::rt) fn validate_filter_at(
        commands: &[DeckPart],
        at: SessionFrame,
    ) -> Result<(), DeckRefusal> {
        match program_in(commands) {
            Some(program) if program.start() != at => Err(DeckRefusal::FilterProgramClock),
            _ => Ok(()),
        }
    }

    /// A manual knob move or a new program ends the running program where it
    /// stands; the filter keeps the position the program last applied.
    pub(in crate::rt) fn interrupt_filter(
        &mut self,
        due: &mut Due<'_, DeckProtocol>,
        part: &DeckPart,
    ) {
        if !cancels(part) {
            return;
        }
        let state = &mut self.filter_programs.knob;
        let previous = state.program.active();
        state.program = ProgramState::Manual;
        if matches!(part, DeckPart::Mix(_)) {
            state.authority = Some(due.seq());
        }
        if let Some(seq) = previous
            && due
                .committed_mut(seq)
                .is_some_and(|commands| self.output_finished(commands, seq))
        {
            due.complete(seq, ());
        }
    }

    pub(in crate::rt) fn begin_filter_program(&mut self, seq: Seq) {
        self.filter_programs.knob.program = ProgramState::Pending(seq);
    }

    pub(in crate::rt) fn finish_filter_programs(
        &mut self,
        level: &mut LevelInbox<'_, DeckProtocol>,
        at: SessionFrame,
    ) {
        let state = &mut self.filter_programs.knob;
        let Some(seq) = state.program.active() else {
            return;
        };
        let program = program_in(level.committed(seq).expect("active filter batch"))
            .expect("active filter program");
        if at >= program.end() {
            state.program = ProgramState::Held(seq);
            self.complete_output(level, seq);
        } else {
            state.program = ProgramState::Running(seq);
        }
    }

    pub(in crate::rt) fn cancel_filter_epoch(&mut self, level: &mut LevelInbox<'_, DeckProtocol>) {
        let state = &mut self.filter_programs.knob;
        let previous = state.program.active();
        state.program = ProgramState::Manual;
        if let Some(seq) = previous {
            self.complete_output(level, seq);
        }
    }
}

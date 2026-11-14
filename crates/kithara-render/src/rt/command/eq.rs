use kithara_command::{Due, LevelInbox, ScopeId, Seq};
use kithara_effects::GainDb;
use kithara_signal::{SessionEpoch, SessionFrame};
use num_traits::ToPrimitive;

use super::{
    super::processor::Deck,
    parameter::{ParameterState, ProgramState},
};
use crate::bridge::{
    DeckEqChange, DeckPart, DeckProtocol, DeckRefusal, EqBinding, EqGainProgram, EqSnapshot,
    ParameterCurve,
};

pub(in crate::rt) type EqCurves<'a> =
    [Option<(usize, &'a ParameterCurve)>; EqGainProgram::MAX_ACTIVE];

/// Parameter metadata only; the inbox retains every owning program.
pub(in crate::rt) struct EqPrograms {
    scope: ScopeId,
    layout: Option<Seq>,
    fields: Vec<ParameterState>,
}

impl EqPrograms {
    pub(in crate::rt) fn new(scope: ScopeId, bands: usize) -> Self {
        Self {
            scope,
            layout: None,
            fields: vec![ParameterState::default(); bands],
        }
    }

    pub(in crate::rt) fn has_seq(&self, seq: Seq) -> bool {
        self.fields
            .iter()
            .any(|field| field.program.active() == Some(seq))
    }

    pub(in crate::rt) fn begin(&mut self, band: usize, seq: Seq) {
        self.fields.get_mut(band).expect("admitted EQ band").program = ProgramState::Pending(seq);
    }

    pub(in crate::rt) fn replace_layout(&mut self, seq: Seq) {
        self.layout = Some(seq);
        self.fields.fill(ParameterState::default());
    }

    pub(in crate::rt) fn publish(&self, snapshot: &mut EqSnapshot, epoch: Option<SessionEpoch>) {
        snapshot.set_binding(epoch.map(|epoch| EqBinding::new(self.scope, self.layout, epoch)));
        for (band, field) in self.fields.iter().enumerate() {
            snapshot.publish_program(band, field.program.applied(), field.authority);
        }
    }

    pub(in crate::rt) fn curves<'a>(
        &self,
        level: &'a LevelInbox<'_, DeckProtocol>,
    ) -> EqCurves<'a> {
        let mut curves = [None; EqGainProgram::MAX_ACTIVE];
        let mut next = 0;
        for (band, field) in self.fields.iter().enumerate() {
            let Some(seq) = field.program.active() else {
                continue;
            };
            let commands = level
                .committed(seq)
                .expect("active EQ owns a committed batch");
            let program = program_in(commands, band).expect("active EQ owns its band program");
            *curves.get_mut(next).expect("admitted active EQ capacity") =
                Some((band, program.curve()));
            next = next.saturating_add(1);
        }
        curves
    }
}

fn program_in(commands: &[DeckPart], band: usize) -> Option<&EqGainProgram> {
    commands.iter().find_map(|part| match part {
        DeckPart::EqProgram { program, .. } if program.band() == band => Some(program),
        _ => None,
    })
}

pub(in crate::rt) fn gain_at(curve: &ParameterCurve, at: SessionFrame) -> GainDb {
    let value = curve.value_at(at);
    GainDb::checked(value.to_f32().expect("admitted gain fits f32"))
        .expect("monotone admitted gain remains in range")
}

fn cancels(part: &DeckPart, band: usize) -> bool {
    match part {
        DeckPart::Eq(DeckEqChange::Layout(_)) => true,
        DeckPart::Eq(DeckEqChange::Gain { band: target, .. }) => *target == band,
        DeckPart::EqProgram { program, .. } => program.band() == band,
        _ => false,
    }
}

impl Deck {
    pub(in crate::rt) fn validate_eq_programs(
        &self,
        commands: &[DeckPart],
    ) -> Result<(), DeckRefusal> {
        if !commands
            .iter()
            .any(|part| matches!(part, DeckPart::EqProgram { .. }))
        {
            return Ok(());
        }
        for (index, part) in commands.iter().enumerate() {
            let DeckPart::EqProgram {
                binding,
                authority,
                program,
            } = part
            else {
                continue;
            };
            let band = program.band();
            if self.epoch != Some(binding.epoch()) {
                return Err(DeckRefusal::AxisRestarted);
            }
            let field = self
                .eq_programs
                .fields
                .get(band)
                .ok_or(DeckRefusal::EqUnavailable { band })?;
            if binding.scope() != self.eq_programs.scope
                || binding.instance() != self.eq_programs.layout
                || *authority != field.authority
            {
                return Err(DeckRefusal::EqOutdated { band });
            }
            if !self.render.accepts_eq_program(band) {
                return Err(DeckRefusal::EqUnavailable { band });
            }
            if commands.iter().any(|part| matches!(part, DeckPart::Eq(DeckEqChange::Layout(_))))
                || commands.iter().any(|part| matches!(part,
                    DeckPart::Eq(DeckEqChange::Gain { band: target, .. }) if *target == band))
                || commands.get(..index).expect("enumerated command prefix").iter().any(|part|
                    matches!(part, DeckPart::EqProgram { program, .. } if program.band() == band))
            {
                return Err(DeckRefusal::EqOutdated { band });
            }
        }
        if self.eq_programs_after(commands) + self.filter_programs_after(commands)
            > EqGainProgram::MAX_ACTIVE
        {
            return Err(DeckRefusal::EqProgramCapacity);
        }
        Ok(())
    }

    /// Active EQ programs after `commands` apply.
    pub(in crate::rt) fn eq_programs_after(&self, commands: &[DeckPart]) -> usize {
        self.eq_programs
            .fields
            .iter()
            .enumerate()
            .filter(|(band, field)| {
                commands.iter().any(|part| {
                    matches!(part,
                DeckPart::EqProgram { program, .. } if program.band() == *band)
                }) || (field.program.active().is_some()
                    && !commands.iter().any(|part| cancels(part, *band)))
            })
            .count()
    }

    pub(in crate::rt) fn validate_eq_at(
        commands: &[DeckPart],
        at: SessionFrame,
    ) -> Result<(), DeckRefusal> {
        for part in commands {
            if let DeckPart::EqProgram { program, .. } = part
                && program.start() != at
            {
                return Err(DeckRefusal::EqProgramClock {
                    band: program.band(),
                });
            }
        }
        Ok(())
    }

    pub(in crate::rt) fn interrupt_eq(&mut self, due: &mut Due<'_, DeckProtocol>, part: &DeckPart) {
        if !matches!(part, DeckPart::Eq(_) | DeckPart::EqProgram { .. }) {
            return;
        }
        let at = due.at();
        let current = due.seq();
        let mut completed = [None; EqGainProgram::MAX_ACTIVE];
        let mut next = 0;
        for band in 0..self.eq_programs.fields.len() {
            if !cancels(part, band) {
                continue;
            }
            let field = *self.eq_programs.fields.get(band).expect("configured band");
            if let ProgramState::Running(seq) = field.program {
                let gain = gain_at(
                    program_in(
                        due.committed_mut(seq).expect("active EQ owns its batch"),
                        band,
                    )
                    .expect("active EQ owns its program")
                    .curve(),
                    at,
                );
                self.render.set_eq_gain_explicit(band, gain);
            }
            let state = self
                .eq_programs
                .fields
                .get_mut(band)
                .expect("configured band");
            state.program = ProgramState::Manual;
            if matches!(part, DeckPart::Eq(DeckEqChange::Gain { .. })) {
                state.authority = Some(current);
            }
            if let Some(seq) = field.program.active() {
                *completed.get_mut(next).expect("admitted active capacity") = Some(seq);
                next = next.saturating_add(1);
            }
        }
        for seq in completed.into_iter().flatten() {
            if due
                .committed_mut(seq)
                .is_some_and(|commands| self.output_finished(commands, seq))
            {
                due.complete(seq, ());
            }
        }
    }

    pub(in crate::rt) fn finish_eq_programs(
        &mut self,
        level: &mut LevelInbox<'_, DeckProtocol>,
        at: SessionFrame,
    ) {
        let mut completed = [None; EqGainProgram::MAX_ACTIVE];
        let mut next = 0;
        for (band, field) in self.eq_programs.fields.iter_mut().enumerate() {
            let Some(seq) = field.program.active() else {
                continue;
            };
            let program = program_in(level.committed(seq).expect("active EQ batch"), band)
                .expect("active EQ band program");
            field.program = if at >= program.end() {
                *completed.get_mut(next).expect("admitted active capacity") = Some(seq);
                next = next.saturating_add(1);
                ProgramState::Held(seq)
            } else {
                ProgramState::Running(seq)
            };
        }
        for seq in completed.into_iter().flatten() {
            self.complete_output(level, seq);
        }
    }

    pub(in crate::rt) fn cancel_eq_epoch(&mut self, level: &mut LevelInbox<'_, DeckProtocol>) {
        let mut completed = [None; EqGainProgram::MAX_ACTIVE];
        let mut next = 0;
        for field in &mut self.eq_programs.fields {
            if let Some(seq) = field.program.active() {
                *completed.get_mut(next).expect("admitted active capacity") = Some(seq);
                next = next.saturating_add(1);
            }
            field.program = ProgramState::Manual;
        }
        for seq in completed.into_iter().flatten() {
            self.complete_output(level, seq);
        }
    }
}

use std::ops::Range;

use kithara_command::{Due, LevelInbox, ScopeId, Seq};
use kithara_effects::GainDb;
use kithara_signal::{SessionEpoch, SessionFrame};
use num_traits::ToPrimitive;

use super::super::processor::Deck;
use crate::bridge::{
    DeckEqChange, DeckPart, DeckProtocol, DeckRefusal, EqBinding, EqGainProgram, EqSnapshot,
    ParameterCurve,
};

pub(in crate::rt) type EqCurves<'a> =
    [Option<(usize, &'a ParameterCurve)>; EqGainProgram::MAX_ACTIVE];

#[derive(Clone, Copy, Default)]
enum Program {
    #[default]
    Manual,
    Pending(Seq),
    Running(Seq),
    Held(Seq),
}

impl Program {
    fn active(self) -> Option<Seq> {
        match self {
            Self::Pending(seq) | Self::Running(seq) => Some(seq),
            Self::Manual | Self::Held(_) => None,
        }
    }

    fn applied(self) -> Option<Seq> {
        match self {
            Self::Running(seq) | Self::Held(seq) => Some(seq),
            Self::Manual | Self::Pending(_) => None,
        }
    }
}

#[derive(Clone, Copy, Default)]
struct Field {
    program: Program,
    authority: Option<Seq>,
}

/// Parameter metadata only; the inbox retains every owning program.
pub(in crate::rt) struct EqPrograms {
    scope: ScopeId,
    layout: Option<Seq>,
    fields: Vec<Field>,
}

impl EqPrograms {
    pub(in crate::rt) fn new(scope: ScopeId, bands: usize) -> Self {
        Self {
            scope,
            layout: None,
            fields: vec![Field::default(); bands],
        }
    }

    pub(in crate::rt) fn has_seq(&self, seq: Seq) -> bool {
        self.fields
            .iter()
            .any(|field| field.program.active() == Some(seq))
    }

    pub(in crate::rt) fn begin(&mut self, band: usize, seq: Seq) {
        self.fields.get_mut(band).expect("admitted EQ band").program = Program::Pending(seq);
    }

    pub(in crate::rt) fn replace_layout(&mut self, seq: Seq) {
        self.layout = Some(seq);
        self.fields.fill(Field::default());
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

fn program_in_mut(commands: &mut [DeckPart], band: usize) -> Option<&mut EqGainProgram> {
    commands.iter_mut().find_map(|part| match part {
        DeckPart::EqProgram { program, .. } if program.band() == band => Some(program),
        _ => None,
    })
}

pub(in crate::rt) fn gain_at(curve: &ParameterCurve, at: SessionFrame) -> GainDb {
    let elapsed = at
        .frames_since(curve.start())
        .expect("the program has started on this axis");
    let (left, right, progression) = curve.segment_at(elapsed);
    let value = if left == right || progression == 0.0 {
        left
    } else if progression == 1.0 {
        right
    } else {
        left + (right - left) * progression
    };
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
                || binding.layout() != self.eq_programs.layout
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
        let active = self
            .eq_programs
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
            .count();
        if active > EqGainProgram::MAX_ACTIVE {
            return Err(DeckRefusal::EqProgramCapacity);
        }
        Ok(())
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
            if let Program::Running(seq) = field.program {
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
            state.program = Program::Manual;
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

    pub(in crate::rt) fn finish_range(
        &mut self,
        level: &mut LevelInbox<'_, DeckProtocol>,
        start: SessionFrame,
        range: Range<usize>,
        input: Option<[&mut [f32]; 2]>,
    ) {
        let at = SessionFrame::new(
            i64::from(start).saturating_add(i64::try_from(range.start).expect("prepared frame")),
        );
        let animated = {
            let curves = self.eq_programs.curves(level);
            let animated = curves.iter().any(Option::is_some);
            if let Some([left, right]) = input {
                if animated {
                    self.render.finish_eq_curves(left, right, at, &curves);
                } else {
                    self.render.finish(left, right);
                }
            } else {
                if animated {
                    self.render.advance_eq_curves(at, range.len(), &curves);
                }
                self.render.idle();
            }
            animated
        };
        if !animated {
            return;
        }
        let Some(last) = range.end.checked_sub(1) else {
            return;
        };
        let at = SessionFrame::new(
            i64::from(start).saturating_add(i64::try_from(last).expect("prepared frame")),
        );
        let mut completed = [None; EqGainProgram::MAX_ACTIVE];
        let mut next = 0;
        for (band, field) in self.eq_programs.fields.iter_mut().enumerate() {
            let Some(seq) = field.program.active() else {
                continue;
            };
            let program = program_in_mut(level.committed_mut(seq).expect("active EQ batch"), band)
                .expect("active EQ band program");
            let gain = self
                .render
                .eq_gain(band)
                .expect("rendered EQ band remains installed");
            program.record_rendered(at, gain);
            field.program = if at >= program.end() {
                *completed.get_mut(next).expect("admitted active capacity") = Some(seq);
                next = next.saturating_add(1);
                Program::Held(seq)
            } else {
                Program::Running(seq)
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
            field.program = Program::Manual;
        }
        for seq in completed.into_iter().flatten() {
            self.complete_output(level, seq);
        }
    }
}

use std::ops::RangeInclusive;

use kithara_command::{Due, LevelInbox, Seq};
use kithara_effects::filter::{FilterParameter, FilterSettingsChange};
use kithara_signal::SessionFrame;

use super::{
    super::processor::Deck,
    parameter::{ParameterState, ProgramState},
};
use crate::bridge::{
    DeckFilterChange, DeckPart, DeckProtocol, DeckRefusal, EqGainProgram, FilterProgram,
    FilterSnapshot,
};

pub(in crate::rt) type FilterCurves<'a> = [Option<&'a FilterProgram>; 3];

#[derive(Default)]
pub(in crate::rt) struct FilterPrograms {
    fields: [ParameterState; 3],
}

impl FilterPrograms {
    pub(in crate::rt) fn has_seq(&self, seq: Seq) -> bool {
        self.fields
            .iter()
            .any(|field| field.program.active() == Some(seq))
    }

    pub(in crate::rt) fn active_count(&self) -> usize {
        self.fields
            .iter()
            .filter(|field| field.program.active().is_some())
            .count()
    }

    pub(in crate::rt) fn publish(&self, snapshot: &mut FilterSnapshot) {
        for field in FilterParameter::ALL {
            let state = self.fields[field.index()];
            snapshot.publish_program(field, state.program.applied(), state.authority);
        }
    }

    pub(in crate::rt) fn curves<'a>(
        &self,
        level: &'a LevelInbox<'_, DeckProtocol>,
    ) -> FilterCurves<'a> {
        FilterParameter::ALL.map(|field| {
            self.fields[field.index()].program.active().map(|seq| {
                program_in(
                    level.committed(seq).expect("active Filter owns its batch"),
                    field,
                )
                .expect("active Filter owns its field program")
            })
        })
    }
}

fn program_in(commands: &[DeckPart], field: FilterParameter) -> Option<&FilterProgram> {
    commands.iter().find_map(|part| match part {
        DeckPart::FilterProgram { program, .. } if program.parameter() == field => Some(program),
        _ => None,
    })
}

impl Deck {
    pub(in crate::rt) fn take_filter_due(
        &mut self,
        mut due: Due<'_, DeckProtocol>,
        context_valid: bool,
    ) {
        let binding = match due.commands() {
            [DeckPart::Filter { binding, .. } | DeckPart::FilterProgram { binding, .. }] => {
                *binding
            }
            _ => {
                due.refuse(DeckRefusal::FilterBatch);
                return;
            }
        };
        if !context_valid || self.epoch != Some(binding.epoch()) {
            due.refuse(DeckRefusal::AxisRestarted);
            return;
        }
        if binding != self.render.filter_binding(self.scope, binding.epoch()) {
            due.refuse(DeckRefusal::FilterOutdated);
            return;
        }
        let takeover = match self.validate_filter_due(&mut due) {
            Ok(takeover) => takeover,
            Err(reason) => {
                due.refuse(reason);
                return;
            }
        };
        if let Some(point) = takeover {
            self.render.set_filter_explicit(point);
        }
        let seq = due.seq();
        let part = due.commands_mut().remove(0);
        match part {
            DeckPart::FilterProgram {
                binding,
                authority,
                program,
            } => {
                let field = program.parameter();
                self.cancel_filter(&mut due, Some(field), None);
                self.filter_programs.fields[field.index()].program = ProgramState::Pending(seq);
                due.commands_mut().push(DeckPart::FilterProgram {
                    binding,
                    authority,
                    program,
                });
                due.commit();
            }
            DeckPart::Filter { binding, change } => {
                let (field, authority) = match &change {
                    DeckFilterChange::Configure(value) => {
                        (Some(FilterParameter::of(*value)), Some(seq))
                    }
                    DeckFilterChange::Reset => (None, Some(seq)),
                    DeckFilterChange::Install(_) | DeckFilterChange::Clear => (None, None),
                };
                let returned = self
                    .render
                    .change_filter(change, binding, seq, self.sample_rate)
                    .expect("Filter geometry and combined trajectories admitted before mutation");
                self.cancel_filter(&mut due, field, authority);
                if let Some(returned) = returned {
                    due.commands_mut().push(returned);
                }
                due.apply(());
            }
            _ => unreachable!("single Filter admission"),
        }
    }

    fn validate_filter_due(
        &self,
        due: &mut Due<'_, DeckProtocol>,
    ) -> Result<Option<FilterSettingsChange>, DeckRefusal> {
        let (field, range) = match due.commands() {
            [
                DeckPart::FilterProgram {
                    authority, program, ..
                },
            ] => {
                let field = program.parameter();
                let state = self.filter_programs.fields[field.index()];
                if *authority != state.authority {
                    return Err(DeckRefusal::FilterOutdated);
                }
                if program.start() != due.at() {
                    return Err(DeckRefusal::FilterProgramClock);
                }
                let active = self.eq_programs.active_count()
                    + self.filter_programs.active_count()
                    + usize::from(state.program.active().is_none());
                if active > EqGainProgram::MAX_ACTIVE {
                    return Err(DeckRefusal::FilterProgramCapacity);
                }
                (field, program.curve().value_range())
            }
            [
                DeckPart::Filter {
                    change: DeckFilterChange::Configure(value),
                    ..
                },
            ] => (FilterParameter::of(*value), value.value()..=value.value()),
            [
                DeckPart::Filter {
                    change: DeckFilterChange::Install(filter),
                    ..
                },
            ] => {
                return if filter.accepts_spec(kithara_signal::AudioSpec::new(2, self.sample_rate)) {
                    Ok(None)
                } else {
                    Err(DeckRefusal::FilterShape)
                };
            }
            [
                DeckPart::Filter {
                    change: DeckFilterChange::Clear,
                    ..
                },
            ] => return Ok(None),
            [
                DeckPart::Filter {
                    change: DeckFilterChange::Reset,
                    ..
                },
            ] => {
                return if self.render.filter_ready(self.sample_rate) {
                    Ok(None)
                } else {
                    Err(DeckRefusal::FilterShape)
                };
            }
            _ => unreachable!("single Filter admission"),
        };
        let (ranges, takeover) = self.filter_ranges_at(due, field, range)?;
        self.render.admit_filter_ranges(self.sample_rate, &ranges)?;
        Ok(takeover)
    }

    fn filter_ranges_at(
        &self,
        due: &mut Due<'_, DeckProtocol>,
        addressed: FilterParameter,
        range: RangeInclusive<f64>,
    ) -> Result<([RangeInclusive<f64>; 3], Option<FilterSettingsChange>), DeckRefusal> {
        let mut ranges = self
            .render
            .filter_ranges()
            .ok_or(DeckRefusal::FilterShape)?;
        ranges[addressed.index()] = range;
        let at = due.at();
        let mut takeover = None;
        for field in FilterParameter::ALL {
            let state = self.filter_programs.fields[field.index()];
            let Some(seq) = state.program.active() else {
                continue;
            };
            let program = program_in(due.committed_mut(seq).expect("active Filter batch"), field)
                .expect("active Filter program");
            if field != addressed {
                ranges[field.index()] = program.curve().value_range();
            } else if matches!(state.program, ProgramState::Running(_)) {
                let point = program.change_at(at);
                let range = &ranges[field.index()];
                ranges[field.index()] =
                    (*range.start()).min(point.value())..=(*range.end()).max(point.value());
                takeover = Some(point);
            }
        }
        Ok((ranges, takeover))
    }

    fn cancel_filter(
        &mut self,
        due: &mut Due<'_, DeckProtocol>,
        addressed: Option<FilterParameter>,
        authority: Option<Seq>,
    ) {
        let mut completed = [None; 3];
        for field in FilterParameter::ALL {
            if addressed.is_some_and(|addressed| addressed != field) {
                continue;
            }
            let state = &mut self.filter_programs.fields[field.index()];
            completed[field.index()] = state.program.active();
            state.program = ProgramState::Manual;
            if authority.is_some() || addressed.is_none() {
                state.authority = authority;
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

    pub(in crate::rt) fn finish_filter_programs(
        &mut self,
        level: &mut LevelInbox<'_, DeckProtocol>,
        at: SessionFrame,
    ) {
        let mut completed = [None; 3];
        for field in FilterParameter::ALL {
            let state = &mut self.filter_programs.fields[field.index()];
            let Some(seq) = state.program.active() else {
                continue;
            };
            let program = program_in(level.committed(seq).expect("active Filter batch"), field)
                .expect("active Filter program");
            state.program = if at >= program.end() {
                completed[field.index()] = Some(seq);
                ProgramState::Held(seq)
            } else {
                ProgramState::Running(seq)
            };
        }
        for seq in completed.into_iter().flatten() {
            self.complete_output(level, seq);
        }
    }

    pub(in crate::rt) fn cancel_filter_epoch(&mut self, level: &mut LevelInbox<'_, DeckProtocol>) {
        let mut completed = [None; 3];
        for (index, state) in self.filter_programs.fields.iter_mut().enumerate() {
            completed[index] = state.program.active();
            state.program = ProgramState::Manual;
        }
        for seq in completed.into_iter().flatten() {
            self.complete_output(level, seq);
        }
    }
}

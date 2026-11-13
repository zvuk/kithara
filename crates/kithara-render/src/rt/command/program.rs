use kithara_command::{Due, LevelInbox, ScopeId, Seq};
use kithara_signal::{SessionEpoch, SessionFrame};

use super::{
    super::processor::Deck,
    parameter::{ParameterState, ProgramState},
};
use crate::bridge::{
    DeckEqChange, DeckFxChange, DeckMixSettingsChange, DeckParameter, DeckPart, DeckProgram,
    DeckProtocol, DeckRefusal, DeckSnapshot, LayoutRevision, ParameterCurve, ParameterSnapshot,
    Returned,
};

pub(in crate::rt) struct DeckPrograms {
    scope: ScopeId,
    layout: Option<Seq>,
    units: Vec<Option<Seq>>,
    fields: Vec<ParameterState>,
    values: Vec<Box<[f64]>>,
    written: Vec<bool>,
}

impl DeckPrograms {
    pub(in crate::rt) fn new(scope: ScopeId, bands: usize, units: usize, capacity: usize) -> Self {
        let count = bands.saturating_add(4);
        Self {
            scope,
            layout: None,
            units: vec![None; units],
            fields: vec![ParameterState::default(); count],
            values: (0..count)
                .map(|_| vec![0.0; capacity].into_boxed_slice())
                .collect(),
            written: vec![false; count],
        }
    }

    pub(in crate::rt) fn has_seq(&self, seq: Seq) -> bool {
        self.fields
            .iter()
            .any(|field| field.program.active() == Some(seq))
    }

    pub(in crate::rt) fn begin(&mut self, parameter: DeckParameter, seq: Seq) {
        if let Some(field) = self.fields.get_mut(parameter.index()) {
            field.begin(seq);
        }
    }

    pub(in crate::rt) fn write_curve(
        &mut self,
        index: usize,
        curve: &ParameterCurve,
        at: SessionFrame,
        frames: usize,
    ) {
        if let (Some(field), Some(values), Some(written)) = (
            self.fields.get_mut(index),
            self.values.get_mut(index),
            self.written.get_mut(index),
        ) {
            if let Some(output) = values.get_mut(..frames) {
                field.write(curve, at, output);
                *written = true;
            }
        }
    }

    pub(in crate::rt) fn write(
        &mut self,
        level: &LevelInbox<'_, DeckProtocol>,
        at: SessionFrame,
        frames: usize,
    ) {
        self.written.fill(false);
        for index in 0..self.fields.len() {
            let seq = self
                .fields
                .get(index)
                .and_then(|field| field.program.active());
            if let Some(curve) = seq
                .and_then(|seq| level.committed(seq))
                .and_then(|commands| program_in(commands, index))
                .map(DeckProgram::curve)
            {
                self.write_curve(index, curve, at, frames);
            }
        }
    }

    pub(in crate::rt) fn any_written(&self) -> bool {
        self.written.iter().any(|written| *written)
    }

    pub(in crate::rt) fn values(&self, index: usize, frames: usize) -> Option<&[f64]> {
        self.written
            .get(index)
            .copied()
            .filter(|written| *written)
            .and_then(|_| self.values.get(index))
            .and_then(|values| values.get(..frames))
    }

    pub(in crate::rt) fn eq_values(
        &self,
        frames: usize,
    ) -> impl Iterator<Item = (usize, &[f64])> + Clone {
        (4..self.fields.len())
            .filter_map(move |index| self.values(index, frames).map(|values| (index - 4, values)))
    }

    pub(in crate::rt) fn publish(
        &self,
        snapshot: &mut DeckSnapshot,
        epoch: Option<SessionEpoch>,
        channels: [f64; 3],
        knob: f64,
    ) {
        snapshot.epoch = epoch;
        snapshot
            .eq
            .set_layout(epoch.map(|_| LayoutRevision::new(self.scope, self.layout)));
        for (unit, revision) in snapshot.fx_layouts.iter_mut().zip(&self.units) {
            *unit = epoch.map(|_| LayoutRevision::new(self.scope, *revision));
        }
        for (index, field) in self.fields.iter().enumerate() {
            if let Some(channel) = snapshot.channels.get_mut(index) {
                *channel = ParameterSnapshot::new(
                    channels[index],
                    field.program.applied(),
                    field.authority,
                );
            } else if index == 3 {
                snapshot
                    .filter
                    .publish(knob, field.program.applied(), field.authority);
            } else {
                snapshot
                    .eq
                    .publish_program(index - 4, field.program.applied(), field.authority);
            }
        }
    }
}

fn program_in(commands: &[DeckPart], index: usize) -> Option<&DeckProgram> {
    commands.iter().find_map(|part| match part {
        DeckPart::Program { program, .. } | DeckPart::Returned(Returned::Program(program))
            if program.parameter().index() == index =>
        {
            Some(program)
        }
        _ => None,
    })
}

fn manual(part: &DeckPart) -> Option<DeckParameter> {
    match part {
        DeckPart::Mix(change) => match change {
            DeckMixSettingsChange::Trim(_) => Some(DeckParameter::Trim),
            DeckMixSettingsChange::Volume(_) => Some(DeckParameter::Volume),
            DeckMixSettingsChange::Muted(_) => None,
            DeckMixSettingsChange::Crossfader(_) => Some(DeckParameter::Crossfader),
            DeckMixSettingsChange::Filter(_) => Some(DeckParameter::Filter),
        },
        _ => None,
    }
}

fn cancels(part: &DeckPart, index: usize) -> bool {
    match part {
        DeckPart::Eq(DeckEqChange::Layout(_)) => index >= 4,
        DeckPart::Eq(DeckEqChange::Gain { band, .. }) => index == band.saturating_add(4),
        DeckPart::Program { program, .. } => index == program.parameter().index(),
        _ => manual(part).is_some_and(|parameter| parameter.index() == index),
    }
}

impl Deck {
    pub(in crate::rt) fn validate_programs(
        &self,
        commands: &[DeckPart],
    ) -> Result<(), DeckRefusal> {
        if commands.iter().any(|part| matches!(part, DeckPart::Eq(DeckEqChange::Layout(layout)) if layout.band_count() > self.programs.fields.len().saturating_sub(4))) { return Err(DeckRefusal::EqLayoutCapacity); }
        for (index, part) in commands.iter().enumerate() {
            let DeckPart::Program {
                epoch,
                authority,
                program,
            } = part
            else {
                continue;
            };
            let parameter = program.parameter();
            if self.epoch != Some(*epoch) {
                return Err(DeckRefusal::AxisRestarted);
            }
            let field = self
                .programs
                .fields
                .get(parameter.index())
                .ok_or(DeckRefusal::ParameterUnavailable { parameter })?;
            if let DeckParameter::EqBand { band, layout } = parameter {
                if layout != LayoutRevision::new(self.programs.scope, self.programs.layout)
                    || commands
                        .iter()
                        .any(|part| matches!(part, DeckPart::Eq(DeckEqChange::Layout(_))))
                {
                    return Err(DeckRefusal::ParameterOutdated { parameter });
                }
                if !self.render.accepts_eq_program(band) {
                    return Err(DeckRefusal::ParameterUnavailable { parameter });
                }
            }
            if *authority != field.authority
                || commands.iter().any(|part| {
                    !matches!(part, DeckPart::Program { .. }) && cancels(part, parameter.index())
                })
                || commands.get(..index).is_some_and(|prefix| {
                    prefix.iter().any(|part| cancels(part, parameter.index()))
                })
            {
                return Err(DeckRefusal::ParameterOutdated { parameter });
            }
        }
        Ok(())
    }

    pub(in crate::rt) fn validate_program_at(
        commands: &[DeckPart],
        at: SessionFrame,
    ) -> Result<(), DeckRefusal> {
        for part in commands {
            if let DeckPart::Program { program, .. } = part {
                if program.start() != at {
                    return Err(DeckRefusal::ProgramClock {
                        parameter: program.parameter(),
                    });
                }
            }
        }
        Ok(())
    }

    pub(in crate::rt) fn interrupt_programs(
        &mut self,
        due: &mut Due<'_, DeckProtocol>,
        part: &DeckPart,
    ) {
        for index in 0..self.programs.fields.len() {
            if !cancels(part, index) {
                continue;
            }
            let Some(field) = self.programs.fields.get_mut(index) else {
                continue;
            };
            let previous = field.program.active();
            field.program = ProgramState::Manual;
            if !matches!(part, DeckPart::Program { .. }) {
                field.authority = Some(due.seq());
            }
            if let Some(seq) = previous {
                if due
                    .committed_mut(seq)
                    .is_some_and(|commands| self.output_finished(commands, seq))
                {
                    due.complete(seq, ());
                }
            }
        }
        match part {
            DeckPart::Eq(DeckEqChange::Layout(_)) => {
                self.programs.layout = Some(due.seq());
                if let Some(fields) = self.programs.fields.get_mut(4..) {
                    fields.fill(ParameterState::default());
                }
            }
            DeckPart::Fx(DeckFxChange::Install { unit, .. } | DeckFxChange::Clear { unit }) => {
                if let Some(revision) = self.programs.units.get_mut(*unit) {
                    *revision = Some(due.seq());
                }
            }
            _ => {}
        }
    }

    pub(in crate::rt) fn finish_programs(
        &mut self,
        level: &mut LevelInbox<'_, DeckProtocol>,
        at: SessionFrame,
    ) {
        for index in 0..self.programs.fields.len() {
            let Some(field) = self.programs.fields.get_mut(index) else {
                continue;
            };
            let Some(seq) = field.program.active() else {
                continue;
            };
            let ended = level
                .committed(seq)
                .and_then(|commands| program_in(commands, index))
                .is_some_and(|program| at >= program.end());
            if ended {
                field.program = ProgramState::Held(seq);
                self.complete_output(level, seq);
            }
        }
    }

    pub(in crate::rt) fn cancel_program_epoch(&mut self, level: &mut LevelInbox<'_, DeckProtocol>) {
        for index in 0..self.programs.fields.len() {
            let Some(field) = self.programs.fields.get_mut(index) else {
                continue;
            };
            let previous = field.program.active();
            field.program = ProgramState::Manual;
            if let Some(seq) = previous {
                self.complete_output(level, seq);
            }
        }
    }
}

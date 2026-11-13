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
    values: Vec<Vec<f64>>,
}

impl DeckPrograms {
    pub(in crate::rt) fn new(scope: ScopeId, bands: usize, units: usize, capacity: usize) -> Self {
        let count = DeckParameter::eq_index(bands);
        Self {
            scope,
            layout: None,
            units: vec![None; units],
            fields: vec![ParameterState::default(); count],
            values: (0..count).map(|_| Vec::with_capacity(capacity)).collect(),
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
        if let (Some(field), Some(values)) =
            (self.fields.get_mut(index), self.values.get_mut(index))
        {
            if frames <= values.capacity() {
                values.resize(frames, 0.0);
                field.write(curve, at, values);
            }
        }
    }

    pub(in crate::rt) fn write(
        &mut self,
        level: &LevelInbox<'_, DeckProtocol>,
        at: SessionFrame,
        frames: usize,
    ) {
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
            } else if let Some(values) = self.values.get_mut(index) {
                values.clear();
            }
        }
    }

    pub(in crate::rt) fn any_written(&self) -> bool {
        self.values.iter().any(|values| !values.is_empty())
    }

    pub(in crate::rt) fn values(&self, index: usize, frames: usize) -> Option<&[f64]> {
        self.values
            .get(index)
            .filter(|values| !values.is_empty())
            .and_then(|values| values.get(..frames))
    }

    pub(in crate::rt) fn eq_values(&self) -> &[Vec<f64>] {
        let eq = DeckParameter::EQ_START;
        self.values.get(eq..).unwrap_or_default()
    }

    pub(in crate::rt) fn publish(
        &self,
        snapshot: &mut DeckSnapshot,
        epoch: Option<SessionEpoch>,
        channels: [f64; DeckParameter::CHANNELS],
        knob: f64,
    ) {
        snapshot.epoch = epoch;
        let layout = epoch.map(|_| LayoutRevision::new(self.scope, self.layout));
        snapshot.eq.set_layout(layout);
        for (unit, revision) in snapshot.fx_layouts.iter_mut().zip(&self.units) {
            *unit = epoch.map(|_| LayoutRevision::new(self.scope, *revision));
        }
        for (index, field) in self.fields.iter().enumerate() {
            let program = field.program.applied();
            let authority = field.authority;
            if let Some(channel) = snapshot.channels.get_mut(index) {
                *channel = ParameterSnapshot::new(channels[index], program, authority);
            } else if index == DeckParameter::Filter.index() {
                snapshot.filter.publish(knob, program, authority);
            } else {
                let band = index - DeckParameter::EQ_START;
                snapshot.eq.publish_program(band, program, authority);
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

fn cancels(part: &DeckPart, index: usize) -> bool {
    let parameter = match part {
        DeckPart::Eq(DeckEqChange::Layout(_)) => return index >= DeckParameter::EQ_START,
        DeckPart::Eq(DeckEqChange::Gain { band, .. }) => {
            return index == DeckParameter::eq_index(*band);
        }
        DeckPart::Program { program, .. } => program.parameter(),
        DeckPart::Mix(DeckMixSettingsChange::Trim(_)) => DeckParameter::Trim,
        DeckPart::Mix(DeckMixSettingsChange::Volume(_)) => DeckParameter::Volume,
        DeckPart::Mix(DeckMixSettingsChange::Crossfader(_)) => DeckParameter::Crossfader,
        DeckPart::Mix(DeckMixSettingsChange::Filter(_)) => DeckParameter::Filter,
        _ => return false,
    };
    parameter.index() == index
}

impl Deck {
    pub(in crate::rt) fn validate_programs(
        &self,
        commands: &[DeckPart],
    ) -> Result<(), DeckRefusal> {
        let bands = self.programs.eq_values().len();
        let oversized = commands.iter().any(|part| match part {
            DeckPart::Eq(DeckEqChange::Layout(layout)) => layout.band_count() > bands,
            _ => false,
        });
        if oversized {
            return Err(DeckRefusal::EqLayoutCapacity);
        }
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
                if let Some(fields) = self.programs.fields.get_mut(DeckParameter::EQ_START..) {
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

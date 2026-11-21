use kithara_command::{Due, LevelInbox, ScopeId, Seq};
use kithara_signal::{SessionEpoch, SessionFrame};

use super::{
    super::processor::Deck,
    parameter::{ParameterState, ProgramState},
};
use crate::bridge::{
    DeckEqChange, DeckFxChange, DeckMixSettingsChange, DeckParameter, DeckPart, DeckProgram,
    DeckProtocol, DeckRefusal, DeckSnapshot, FxTarget, LayoutRevision, ParameterCurve,
    ParameterSnapshot, Returned,
};

/// Where each parameter of a deck keeps its program state: the fixed
/// controls, then each EQ band the deck has room for, then each unit's dry/wet.
#[derive(Clone, Copy)]
struct Slots {
    bands: usize,
    units: usize,
}

impl Slots {
    const fn fx_start(self) -> usize {
        DeckParameter::eq_index(self.bands)
    }

    const fn count(self) -> usize {
        self.fx_start() + self.units
    }

    const fn eq(self, index: usize) -> bool {
        index >= DeckParameter::EQ_START && index < self.fx_start()
    }

    fn mix(self, unit: usize) -> Option<usize> {
        (unit < self.units).then(|| self.fx_start() + unit)
    }

    fn of(self, parameter: DeckParameter) -> Option<usize> {
        match parameter {
            DeckParameter::EqBand { band, .. } => {
                (band < self.bands).then(|| DeckParameter::eq_index(band))
            }
            DeckParameter::Fx {
                unit,
                target: FxTarget::Mix,
                ..
            } => self.mix(unit),
            DeckParameter::Fx {
                target: FxTarget::Param(_),
                ..
            } => None,
            fixed => fixed.fixed(),
        }
    }
}

pub(in crate::rt) struct DeckPrograms {
    scope: ScopeId,
    layout: Option<Seq>,
    units: Vec<Option<Seq>>,
    slots: Slots,
    fields: Vec<ParameterState>,
    values: Vec<Vec<f64>>,
}

impl DeckPrograms {
    pub(in crate::rt) fn new(scope: ScopeId, bands: usize, units: usize, capacity: usize) -> Self {
        let slots = Slots { bands, units };
        let count = slots.count();
        Self {
            scope,
            layout: None,
            units: vec![None; units],
            slots,
            fields: vec![ParameterState::default(); count],
            values: (0..count).map(|_| Vec::with_capacity(capacity)).collect(),
        }
    }

    pub(in crate::rt) fn has_seq(&self, seq: Seq) -> bool {
        self.fields
            .iter()
            .any(|field| field.program.active() == Some(seq))
    }

    pub(in crate::rt) fn owner(&self, parameter: DeckParameter) -> Option<Seq> {
        self.slots
            .of(parameter)
            .and_then(|index| self.fields.get(index))
            .and_then(|field| field.program.applied())
    }

    pub(in crate::rt) fn begin(&mut self, parameter: DeckParameter, seq: Seq) {
        if let Some(field) = self
            .slots
            .of(parameter)
            .and_then(|index| self.fields.get_mut(index))
        {
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
            && frames <= values.capacity()
        {
            values.resize(frames, 0.0);
            field.write(curve, at, values);
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
                .and_then(|commands| program_in(self.slots, commands, index))
                .map(DeckProgram::curve)
            {
                self.write_curve(index, curve, at, frames);
            } else if let Some(values) = self.values.get_mut(index) {
                values.clear();
            }
        }
    }

    pub(in crate::rt) fn any_applied(&self) -> bool {
        self.fields
            .iter()
            .any(|field| field.program.applied().is_some())
    }

    pub(in crate::rt) fn values(&self, index: usize, frames: usize) -> Option<&[f64]> {
        self.values
            .get(index)
            .filter(|values| !values.is_empty())
            .and_then(|values| values.get(..frames))
    }

    pub(in crate::rt) fn eq_values(&self) -> &[Vec<f64>] {
        self.values
            .get(DeckParameter::EQ_START..self.slots.fx_start())
            .unwrap_or_default()
    }

    /// The dry/wet a program prepared for `unit` over the next `frames` frames.
    pub(in crate::rt) fn mix_values(&self, unit: usize, frames: usize) -> Option<&[f64]> {
        self.slots
            .mix(unit)
            .and_then(|index| self.values(index, frames))
    }

    pub(in crate::rt) fn publish(
        &self,
        snapshot: &mut DeckSnapshot,
        epoch: Option<SessionEpoch>,
        channels: [f64; DeckParameter::CHANNELS],
        knob: f64,
        mixes: impl Iterator<Item = f64>,
    ) {
        snapshot.epoch = epoch;
        let layout = epoch.map(|_| LayoutRevision::new(self.scope, self.layout));
        snapshot.eq.set_layout(layout);
        for (unit, revision) in snapshot.fx.iter_mut().zip(&self.units) {
            unit.layout = epoch.map(|_| LayoutRevision::new(self.scope, *revision));
        }
        let mut mixes = mixes;
        for (index, field) in self.fields.iter().enumerate() {
            let program = field.program.applied();
            let authority = field.authority;
            if let Some(channel) = snapshot.channels.get_mut(index) {
                *channel = ParameterSnapshot::new(channels[index], program, authority);
            } else if index == DeckParameter::FILTER {
                snapshot.filter.publish(knob, program, authority);
            } else if self.slots.eq(index) {
                let band = index - DeckParameter::EQ_START;
                snapshot.eq.publish_program(band, program, authority);
            } else if let (Some(unit), Some(mix)) = (
                snapshot.fx.get_mut(index - self.slots.fx_start()),
                mixes.next(),
            ) {
                unit.mix = ParameterSnapshot::new(mix, program, authority);
            }
        }
    }
}

fn program_in(slots: Slots, commands: &[DeckPart], index: usize) -> Option<&DeckProgram> {
    commands.iter().find_map(|part| match part {
        DeckPart::Program { program, .. } | DeckPart::Returned(Returned::Program(program))
            if slots.of(program.parameter()) == Some(index) =>
        {
            Some(program)
        }
        _ => None,
    })
}

fn cancels(slots: Slots, part: &DeckPart, index: usize) -> bool {
    let parameter = match part {
        DeckPart::Eq(DeckEqChange::Layout(_) | DeckEqChange::Reset) => {
            return slots.eq(index);
        }
        DeckPart::Eq(DeckEqChange::Gain { band, .. }) => {
            return index == DeckParameter::eq_index(*band);
        }
        DeckPart::Fx(
            DeckFxChange::Mix { unit, .. }
            | DeckFxChange::Install { unit, .. }
            | DeckFxChange::Clear { unit },
        ) => return slots.mix(*unit) == Some(index),
        DeckPart::Program { program, .. } => return slots.of(program.parameter()) == Some(index),
        DeckPart::Mix(DeckMixSettingsChange::Trim(_)) => DeckParameter::TRIM,
        DeckPart::Mix(DeckMixSettingsChange::Volume(_)) => DeckParameter::VOLUME,
        DeckPart::Mix(DeckMixSettingsChange::Crossfader(_)) => DeckParameter::CROSSFADER,
        DeckPart::Mix(DeckMixSettingsChange::Filter(_)) => DeckParameter::FILTER,
        _ => return false,
    };
    parameter == index
}

impl Deck {
    pub(in crate::rt) fn validate_programs(
        &self,
        commands: &[DeckPart],
    ) -> Result<(), DeckRefusal> {
        let bands = self.programs.eq_values().len();
        let slots = self.programs.slots;
        let mut layout_changes = false;
        for part in commands {
            if let DeckPart::Eq(DeckEqChange::Layout(layout)) = part {
                if layout.band_count() > bands {
                    return Err(DeckRefusal::EqLayoutCapacity);
                }
                layout_changes = true;
            }
        }
        for (index, part) in commands.iter().enumerate() {
            if let DeckPart::Eq(DeckEqChange::Kill { band, layout, .. }) = *part {
                self.validate_eq_kill(band, layout, layout_changes)?;
            }
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
            let unavailable = DeckRefusal::ParameterUnavailable { parameter };
            let slot = slots.of(parameter).ok_or(unavailable)?;
            let field = self.programs.fields.get(slot).ok_or(unavailable)?;
            match parameter {
                DeckParameter::EqBand { band, layout } => {
                    self.validate_eq_revision(band, layout, layout_changes)?;
                    if !self.render.accepts_eq_program(band) {
                        return Err(unavailable);
                    }
                }
                DeckParameter::Fx { unit, layout, .. } => {
                    let current = self.programs.units.get(unit).copied().ok_or(unavailable)?;
                    if layout != LayoutRevision::new(self.programs.scope, current) {
                        return Err(DeckRefusal::ParameterOutdated { parameter });
                    }
                    if !self.render.holds_fx(unit) {
                        return Err(unavailable);
                    }
                }
                _ => {}
            }
            if *authority != field.authority
                || commands.iter().any(|part| {
                    !matches!(part, DeckPart::Program { .. }) && cancels(slots, part, slot)
                })
                || commands
                    .get(..index)
                    .is_some_and(|prefix| prefix.iter().any(|part| cancels(slots, part, slot)))
            {
                return Err(DeckRefusal::ParameterOutdated { parameter });
            }
        }
        Ok(())
    }

    fn validate_eq_kill(
        &self,
        band: usize,
        layout: LayoutRevision,
        layout_changes: bool,
    ) -> Result<(), DeckRefusal> {
        self.validate_eq_revision(band, layout, layout_changes)?;
        if band >= self.render.eq_bands() {
            return Err(DeckRefusal::ParameterUnavailable {
                parameter: DeckParameter::EqBand { band, layout },
            });
        }
        Ok(())
    }

    fn validate_eq_revision(
        &self,
        band: usize,
        layout: LayoutRevision,
        layout_changes: bool,
    ) -> Result<(), DeckRefusal> {
        if layout_changes
            || layout != LayoutRevision::new(self.programs.scope, self.programs.layout)
        {
            return Err(DeckRefusal::ParameterOutdated {
                parameter: DeckParameter::EqBand { band, layout },
            });
        }
        Ok(())
    }

    pub(in crate::rt) fn validate_program_at(
        commands: &[DeckPart],
        at: SessionFrame,
    ) -> Result<(), DeckRefusal> {
        for part in commands {
            if let DeckPart::Program { program, .. } = part
                && program.start() != at
            {
                return Err(DeckRefusal::ProgramClock {
                    parameter: program.parameter(),
                });
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
            if !cancels(self.programs.slots, part, index) {
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
            if let Some(seq) = previous
                && due
                    .committed_mut(seq)
                    .is_some_and(|commands| self.output_finished(commands, seq))
            {
                due.complete(seq, ());
            }
        }
        match part {
            DeckPart::Eq(DeckEqChange::Layout(_)) => {
                self.programs.layout = Some(due.seq());
                let eq = DeckParameter::EQ_START..self.programs.slots.fx_start();
                if let Some(fields) = self.programs.fields.get_mut(eq) {
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
                .and_then(|commands| program_in(self.programs.slots, commands, index))
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

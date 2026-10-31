use kithara_audio::TrackFailureKind;
use kithara_command::{Due, Seq};
use kithara_effects::{
    AudioEffect, EffectAutomation, EffectChange, EffectParameter, EffectPrepareError,
    apply_automated_effects, apply_effects,
};
use kithara_signal::{AudioChunk, FrameCount};

use super::{Lane, LaneApplied, LaneCommand, LaneFrame, LaneProtocol, LaneState};
use crate::bridge::EffectProgram;

#[derive(Clone, Copy)]
pub(super) struct RunningEffect {
    seq: Seq,
    parameter: EffectParameter,
    start: LaneFrame,
    end: u64,
    next: u64,
    applied: Option<EffectChange>,
}

fn program(commands: &[LaneCommand]) -> &EffectProgram {
    let [LaneCommand::EffectProgram { program, .. }] = commands else {
        unreachable!("an active parameter owns a single-program committed batch")
    };
    program
}

fn result(commands: &mut [LaneCommand], value: Option<EffectChange>) {
    let [LaneCommand::EffectProgram { applied, .. }] = commands else {
        unreachable!("an active parameter retains its committed batch")
    };
    *applied = value;
}

fn receipt(latency: FrameCount) -> LaneApplied {
    LaneApplied {
        source_basis: None,
        engine_latency: latency,
        ready: None,
    }
}

impl LaneState {
    pub(super) fn start_program(
        &mut self,
        due: &mut Due<'_, LaneProtocol>,
        chain: &[Box<dyn AudioEffect>],
        latency: FrameCount,
    ) -> Result<(), TrackFailureKind> {
        let program = program(due.commands());
        let parameter = program.parameter();
        if chain.iter().any(|effect| !effect.preserves_frames())
            || chain
                .get(parameter.target().effect())
                .is_none_or(|effect| !effect.admits_automation(parameter, program.value_range()))
        {
            return Err(TrackFailureKind::Render);
        }
        let slot = self
            .programs
            .iter()
            .position(|run| run.is_some_and(|run| run.parameter == parameter))
            .or_else(|| self.programs.iter().position(Option::is_none))
            .ok_or(TrackFailureKind::Render)?;
        let duration = program
            .end()
            .frames_since(program.start())
            .ok_or(TrackFailureKind::Render)?;
        let start = due.at();
        let end = start
            .frame
            .checked_add(duration)
            .ok_or(TrackFailureKind::Render)?;
        let next = match program.next_offset(0) {
            Some(offset) => start
                .frame
                .checked_add(offset)
                .ok_or(TrackFailureKind::Render)?,
            None => u64::MAX,
        };
        self.cancel_parameter(parameter, due, latency);
        self.programs[slot] = Some(RunningEffect {
            seq: due.seq(),
            parameter,
            start,
            end,
            next,
            applied: None,
        });
        Ok(())
    }

    pub(super) fn cancel_parameter(
        &mut self,
        parameter: EffectParameter,
        due: &mut Due<'_, LaneProtocol>,
        latency: FrameCount,
    ) {
        for slot in &mut self.programs {
            if slot.is_some_and(|run| run.parameter == parameter) {
                let run = slot.take().expect("matching active field");
                result(
                    due.committed_mut(run.seq)
                        .expect("running program owns its batch"),
                    run.applied,
                );
                due.complete(run.seq, receipt(latency));
            }
        }
    }
}

impl Lane {
    pub(super) fn program_limit(&self) -> usize {
        self.state
            .programs
            .iter()
            .flatten()
            .map(|run| {
                usize::try_from(run.next.saturating_sub(self.state.cursor.frame))
                    .unwrap_or(usize::MAX)
            })
            .min()
            .unwrap_or(usize::MAX)
    }

    pub(crate) fn cancel_programs(&mut self, latency: FrameCount) {
        for slot in &mut self.state.programs {
            if let Some(run) = slot.take() {
                result(
                    self.inbox
                        .committed_mut(run.seq)
                        .expect("running program owns its batch"),
                    run.applied,
                );
                self.inbox.complete(run.seq, receipt(latency));
            }
        }
    }

    pub(crate) fn apply_effects(
        &mut self,
        effects: &mut [Box<dyn AudioEffect>],
        chunk: AudioChunk,
        latency: FrameCount,
    ) -> Result<Option<AudioChunk>, EffectPrepareError> {
        if self.state.programs.iter().all(Option::is_none) {
            return apply_effects(effects, chunk);
        }
        let frames = chunk.frames();
        let last = self
            .state
            .cursor
            .frame
            .checked_add(
                u64::try_from(frames.saturating_sub(1)).map_err(|_| EffectPrepareError::Shape)?,
            )
            .ok_or(EffectPrepareError::Shape)?;
        let mut controls = [EffectAutomation::new(
            EffectParameter::EqGain { effect: 0, band: 0 },
            0.0,
            0.0,
            kithara_dsp::curve::CurveShape::HOLD,
            0,
            0,
        )
        .expect("constant neutral span"); EffectProgram::MAX_ACTIVE];
        let count = self.state.programs.iter().flatten().count();
        for (index, run) in self.state.programs.iter().flatten().enumerate() {
            if run.start.segment != self.state.cursor.segment {
                return Err(EffectPrepareError::Shape);
            }
            let curve = program(
                self.inbox
                    .committed_mut(run.seq)
                    .expect("running program owns its knots"),
            );
            let elapsed = self
                .state
                .cursor
                .frame
                .checked_sub(run.start.frame)
                .ok_or(EffectPrepareError::Shape)?;
            controls[index] = curve.automation(elapsed);
        }
        let output = apply_automated_effects(effects, chunk, &controls[..count])?;
        if frames == 0 {
            return Ok(output);
        }
        for slot in &mut self.state.programs {
            let Some(run) = slot else { continue };
            let commands = self
                .inbox
                .committed_mut(run.seq)
                .expect("running program owns its knots");
            let curve = program(commands);
            run.applied = Some(curve.change(last - run.start.frame));
            run.next = curve
                .next_offset(last.saturating_add(1) - run.start.frame)
                .map_or(u64::MAX, |offset| run.start.frame.saturating_add(offset));
            if last >= run.end {
                let run = slot.take().expect("completed active parameter");
                result(commands, run.applied);
                self.inbox.complete(run.seq, receipt(latency));
            }
        }
        Ok(output)
    }
}

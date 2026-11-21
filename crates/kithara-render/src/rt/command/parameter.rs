use kithara_command::Seq;
use kithara_signal::SessionFrame;

use crate::bridge::{CurveCursor, ParameterCurve};

#[derive(Clone, Copy, Default)]
pub(in crate::rt) enum ProgramState {
    #[default]
    Manual,
    Pending(Seq),
    Running(Seq),
    Held(Seq),
}

impl ProgramState {
    pub(in crate::rt) fn active(self) -> Option<Seq> {
        match self {
            Self::Pending(seq) | Self::Running(seq) => Some(seq),
            Self::Manual | Self::Held(_) => None,
        }
    }

    pub(in crate::rt) fn applied(self) -> Option<Seq> {
        match self {
            Self::Running(seq) | Self::Held(seq) => Some(seq),
            Self::Manual | Self::Pending(_) => None,
        }
    }
}

#[derive(Clone, Copy, Default)]
pub(in crate::rt) struct ParameterState {
    pub(in crate::rt) program: ProgramState,
    pub(in crate::rt) authority: Option<Seq>,
    cursor: CurveCursor,
}

impl ParameterState {
    pub(in crate::rt) fn begin(&mut self, seq: Seq) {
        self.program = ProgramState::Pending(seq);
        self.cursor = CurveCursor::default();
    }

    pub(in crate::rt) fn write(
        &mut self,
        curve: &ParameterCurve,
        at: SessionFrame,
        values: &mut [f64],
    ) {
        if values.is_empty() {
            return;
        }
        curve.write(&mut self.cursor, at, values);
        if let Some(seq) = self.program.active() {
            if SessionFrame::new(
                i64::from(at).saturating_add(
                    i64::try_from(values.len().saturating_sub(1)).unwrap_or(i64::MAX),
                ),
            ) < curve.start()
            {
                return;
            }
            self.program = ProgramState::Running(seq);
        }
    }
}

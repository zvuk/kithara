use kithara_command::Seq;

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
}

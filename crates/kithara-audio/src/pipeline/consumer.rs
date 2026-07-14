/// Consumer-side phase for `Audio<S>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConsumerPhase {
    Buffering,
    Playing,
    SeekPending { epoch: u64 },
    AtEof,
    Failed,
}

impl ConsumerPhase {
    pub(crate) fn is_terminal(self) -> bool {
        matches!(self, Self::AtEof | Self::Failed)
    }
}

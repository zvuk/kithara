mod effect;
mod outcome;
mod reader;

#[cfg(any(test, feature = "mock"))]
pub use effect::AudioEffectMock;
pub use effect::{AudioBlockMut, AudioEffect};
pub(crate) use effect::{
    OutputCredit, TempoBoundaryId, TempoDiscontinuityDebt, TempoDiscontinuityStep, TempoEofDebt,
    TempoEofStep, TempoPrepareRequest, TempoStage, TempoStep,
};
pub use kithara_decode::{DecodeError, DecodeResult};
pub use outcome::{
    ChunkOutcome, PendingReason, PresentationAdvance, PresentationCursor, PresentationPoint,
    ReadOutcome, SeekOutcome,
};
pub use reader::{PcmControl, PcmRead, PcmReader, PcmSession, SeekBegin};
#[cfg(any(test, feature = "mock"))]
pub use reader::{PcmControlMock, PcmReadMock, PcmSessionMock};

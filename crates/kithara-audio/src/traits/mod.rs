mod effect;
mod outcome;
mod reader;

pub use effect::AudioEffect;
#[cfg(any(test, feature = "mock"))]
pub use effect::AudioEffectMock;
pub use kithara_decode::{DecodeError, DecodeResult};
pub use outcome::{ChunkOutcome, PendingReason, ReadOutcome, SeekOutcome};
pub use reader::{PcmControl, PcmRead, PcmReader, PcmSession};
#[cfg(any(test, feature = "mock"))]
pub use reader::{PcmControlMock, PcmReadMock, PcmSessionMock};

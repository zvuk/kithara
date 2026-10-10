mod error;
mod observer;
mod outcome;
mod reader;
mod source;

pub use error::{AudioReadError, FailureSource};
pub use kithara_decode::{DecodeError, DecodeResult};
#[cfg(any(test, feature = "mock"))]
pub use observer::AudioObserverMock;
pub use observer::{AudioObserveError, AudioObserver, AudioObserverRelay, AudioObserverSlot};
pub use outcome::{ChunkOutcome, PendingReason, ReadOutcome, SeekOutcome};
pub use reader::{AudioControl, AudioRead, AudioReader, AudioSession};
#[cfg(any(test, feature = "mock"))]
pub use reader::{AudioControlMock, AudioReadMock, AudioSessionMock};
#[cfg(any(test, feature = "mock"))]
pub use source::AudioSourceMock;
pub use source::{AudioSource, SourceDiscontinuity};

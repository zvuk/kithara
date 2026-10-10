mod pcm_reader;
mod scripted_reader;

#[cfg(all(test, not(target_arch = "wasm32")))]
mod committed;
#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) use committed::{prepared_audio, produced_audio};
pub use pcm_reader::{TEST_PCM_DEFAULT_VALUE, TestPcmReader};
pub use scripted_reader::{Fault, MockReader, SeekSplitCounts};

pub use crate::traits::{
    AudioControlMock, AudioObserverMock, AudioReadMock, AudioSessionMock, AudioSourceMock,
};

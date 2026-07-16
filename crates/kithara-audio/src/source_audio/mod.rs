mod cache;
mod connection;
mod model;
mod reader;
mod tap;

pub(crate) use connection::connect_source_audio;
pub(crate) use model::{SourceAudioCaptureOutcome, SourceAudioTerminal};
pub use model::{SourceAudioDemand, SourceAudioError, SourceAudioReadOutcome, SourceFrameRange};
pub use reader::SourceAudioReader;
pub(crate) use tap::SourceAudioTap;

#[cfg(test)]
mod tests;

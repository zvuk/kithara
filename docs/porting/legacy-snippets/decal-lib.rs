#[cfg(all(feature = "decoder", feature = "output"))]
mod audio_manager;
#[cfg(feature = "decoder")]
pub mod decoder;
#[cfg(feature = "output")]
pub mod output;
#[cfg(all(feature = "decoder", feature = "output"))]
pub use audio_manager::*;
#[cfg(feature = "decoder")]
pub use symphonia;

#[derive(PartialEq, Eq, PartialOrd, Ord, Debug, Clone, Copy)]
pub struct SampleRate(pub u32);

pub(crate) const DEFAULT_SAMPLE_RATE: SampleRate = SampleRate(44100);

#[derive(PartialEq, Eq, PartialOrd, Ord, Debug, Clone, Copy)]
pub struct ChannelCount(pub u16);

mod heuristic;
mod info;
#[cfg(any(test, symphonia_demuxer))]
mod mp3;
mod mp4;
#[cfg(symphonia_demuxer)]
mod probe;
mod trimmer;

pub use heuristic::GaplessMode;
pub use info::{GaplessInfo, GaplessTailCompensation};
#[cfg(feature = "symphonia")]
pub(crate) use mp4::derive_from_edit;
pub use mp4::probe_mp4_gapless;
#[cfg(symphonia_demuxer)]
pub(crate) use probe::scoped_probe;
pub use trimmer::{GaplessOutput, GaplessTrimmer};

mod heuristic;
mod info;
#[cfg(any(test, symphonia_demuxer))]
mod mp3;
mod mp4;
#[cfg(symphonia_demuxer)]
mod probe;
mod trimmer;

pub use heuristic::GaplessMode;
pub(crate) use info::DeclaredGapless;
pub use info::{GaplessInfo, GaplessTailCompensation};
pub(crate) use mp4::probe_mp4_gapless_dyn;
#[cfg(symphonia_demuxer)]
pub(crate) use probe::scoped_probe;
pub use trimmer::{GaplessOutput, GaplessTrimmer};

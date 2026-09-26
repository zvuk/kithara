mod heuristic;
mod info;
#[cfg(any(test, symphonia_demuxer))]
mod mp3;
mod mp4;
#[cfg(symphonia_demuxer)]
mod probe;
mod trimmer;

pub use heuristic::{GaplessMode, SilenceTrimParams};
pub use info::{GaplessInfo, GaplessTailCompensation};
pub use mp4::probe_mp4_gapless;
#[cfg(symphonia_demuxer)]
pub(crate) use probe::scoped_probe;
pub use trimmer::{GaplessOutput, GaplessTrimmer};

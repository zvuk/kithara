mod codec_priming;
mod heuristic;
mod info;
mod mp4;
mod trimmer;

pub use codec_priming::codec_priming_frames;
pub use heuristic::{GaplessMode, SilenceTrimParams};
pub use info::GaplessInfo;
pub use mp4::probe_mp4_gapless;
pub(crate) use mp4::probe_mp4_gapless_dyn;
pub use trimmer::{GaplessOutput, GaplessTrimmer};

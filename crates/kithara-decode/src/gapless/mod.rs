//! Gapless playback support: encoder priming/padding trim.
//!
//! Ported wholesale from PR #64 (production/main). Submodules:
//! - `info` — `GaplessInfo` contract type (frame counts).
//! - `codec_priming` — codec-specific implicit priming defaults.
//! - `heuristic` — `GaplessMode` + silence-trim params.
//! - `mp3` — LAME/Xing tag parsing.
//! - `mp4` — MP4 `iTunSMPB` extraction.
//! - `trimmer` — frame-level trim engine wrapping decoder output.
//!
//! Per-platform codec wiring lands in P4–P6, factory wiring in P7,
//! audio-pipeline wiring in P9. The crate-level re-exports in `lib.rs`
//! make every item reachable from the moment of port so `unreachable_pub`
//! stays quiet without scattered `#[allow]` attributes.

mod codec_priming;
mod heuristic;
mod info;
mod mp3;
mod mp4;
mod trimmer;

pub use codec_priming::codec_priming_frames;
pub use heuristic::{GaplessMode, SilenceTrimParams};
pub use info::GaplessInfo;
pub use mp4::probe_mp4_gapless;
pub(crate) use mp4::probe_mp4_gapless_dyn;
pub use trimmer::{GaplessOutput, GaplessTrimmer};

// `mp3::{LAME_DECODER_DELAY, read_lame_trim}` are wired in via direct
// path (`crate::gapless::mp3::…`) by the symphonia codec MP3 path in
// a follow-up — no need for module-level re-exports until then.

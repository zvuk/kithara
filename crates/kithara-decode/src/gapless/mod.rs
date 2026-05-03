//! Gapless playback support: encoder priming/padding trim.
//!
//! P3 will populate this module with the `heuristic`, `codec_priming`,
//! `trimmer`, and `mp4` extraction submodules. P2 only seeds the
//! `GaplessInfo` contract type so `DecoderTrackInfo` in `types.rs`
//! can reference it without forward-declaration tricks.

pub(crate) mod info;

pub use info::GaplessInfo;

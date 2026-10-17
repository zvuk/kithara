#![forbid(unsafe_code)]
#![recursion_limit = "256"]
#![expect(
    clippy::unwrap_used,
    reason = "integration test crate - unwraps are acceptable in test code"
)]

pub use kithara_integration_tests::bufpool_ext;
use kithara_test_dylib as _;

mod common {
    pub(crate) use kithara_integration_tests::test_defaults;
}
pub use kithara_integration_tests::gapless as gapless_common;

mod bare_deck;
mod crossfade_hls_to_mp3_repeats;
mod hls_seek_middle_no_queue;
mod hls_seek_middle_stress;
mod hls_seek_past_end_terminates;
mod local_seek_hang_iters;
mod non_leading_track_completion;
mod parameter_smoothing;
mod player;
mod quality_switch_continuity;
#[cfg(not(target_arch = "wasm32"))]
mod resource_regressions;
mod seamless_queue_advance;
mod track_replay_after_switch;
mod worker_load;

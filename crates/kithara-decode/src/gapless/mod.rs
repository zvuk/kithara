mod heuristic;
mod info;
#[cfg(any(
    test,
    feature = "symphonia",
    all(feature = "apple", any(target_os = "macos", target_os = "ios"))
))]
mod mp3;
mod mp4;
#[cfg(any(
    feature = "symphonia",
    all(feature = "apple", any(target_os = "macos", target_os = "ios"))
))]
mod probe;
mod trimmer;

pub use heuristic::{GaplessMode, SilenceTrimParams};
pub use info::GaplessInfo;
pub use mp4::probe_mp4_gapless;
#[cfg(any(
    feature = "symphonia",
    all(feature = "apple", any(target_os = "macos", target_os = "ios"))
))]
pub(crate) use probe::scoped_probe;
#[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
pub(crate) use probe::scoped_startup_probe;
pub use trimmer::{GaplessOutput, GaplessTrimmer};

//! Generic audio pipeline that runs in a separate blocking thread.

pub(crate) mod config;
pub(crate) mod fetch;
#[cfg(not(all(
    feature = "apple-fused-src",
    any(target_os = "macos", target_os = "ios")
)))]
pub(crate) mod fused_src_disabled;
#[cfg(all(
    feature = "apple-fused-src",
    any(target_os = "macos", target_os = "ios")
))]
pub(crate) mod fused_src_enabled;
pub(crate) mod gapless;
#[cfg(not(feature = "resample-rubato"))]
pub(crate) mod resampler_stage_disabled;
#[cfg(feature = "resample-rubato")]
pub(crate) mod resampler_stage_enabled;
pub(crate) mod source;
pub(crate) mod track_fsm;

#[cfg(not(all(
    feature = "apple-fused-src",
    any(target_os = "macos", target_os = "ios")
)))]
pub(crate) use fused_src_disabled as fused_src;
#[cfg(all(
    feature = "apple-fused-src",
    any(target_os = "macos", target_os = "ios")
))]
pub(crate) use fused_src_enabled as fused_src;
#[cfg(not(feature = "resample-rubato"))]
pub(crate) use resampler_stage_disabled as resampler_stage;
#[cfg(feature = "resample-rubato")]
pub(crate) use resampler_stage_enabled as resampler_stage;

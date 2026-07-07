//! Generic audio pipeline that runs in a separate blocking thread.

pub(crate) mod config;
pub(crate) mod fetch;
pub(crate) mod fused_src;
pub(crate) mod gapless;
pub(crate) mod resampler_stage;
pub(crate) mod source;
pub(crate) mod track_fsm;

//! Generic audio pipeline that runs in a separate blocking thread.

pub(crate) mod audio;
mod config;
pub(crate) mod source;
pub(crate) mod worker;

pub use audio::Audio;
pub use config::AudioConfig;

//! Generic audio pipeline that runs in a separate blocking thread.

mod audio;
mod config;
mod stream_source;
mod worker;

pub use audio::Audio;
pub use config::AudioConfig;

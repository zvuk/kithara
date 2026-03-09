//! Generic audio pipeline that runs in a separate blocking thread.

pub(crate) mod audio;
#[expect(
    dead_code,
    reason = "used in Step 3 when Audio registers with shared worker"
)]
pub(crate) mod audio_worker;
mod config;
pub(crate) mod source;
pub(crate) mod worker;
#[expect(dead_code, reason = "used by audio_worker, consumed in Step 3")]
pub(crate) mod worker_types;
#[expect(dead_code, reason = "used by audio_worker, consumed in Step 3")]
pub(crate) mod worker_wake;

pub use audio::Audio;
pub use config::AudioConfig;

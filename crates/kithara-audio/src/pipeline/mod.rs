//! Generic audio pipeline that runs in a separate blocking thread.

pub(crate) mod audio;
pub(crate) mod audio_worker;
mod config;
pub(crate) mod source;
pub(crate) mod thread_wake;
pub(crate) mod track_fsm;
pub(crate) mod worker;
pub(crate) mod worker_types;
pub(crate) mod worker_wake;

pub use audio::Audio;
pub use audio_worker::AudioWorkerHandle;
pub use config::AudioConfig;
pub use worker_types::ServiceClass;

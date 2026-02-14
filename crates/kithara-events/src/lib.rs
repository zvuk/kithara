#![forbid(unsafe_code)]

//! Unified event bus for the kithara audio pipeline.

mod bus;
mod event;
mod file;

#[cfg(feature = "audio")]
mod audio;
#[cfg(feature = "hls")]
mod hls;

#[cfg(feature = "audio")]
pub use audio::AudioEvent;
pub use bus::EventBus;
pub use event::Event;
pub use file::FileEvent;
#[cfg(feature = "hls")]
pub use hls::HlsEvent;

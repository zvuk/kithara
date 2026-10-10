#![forbid(unsafe_code)]
#![cfg_attr(all(rtsan, not(rtsan_standalone)), feature(sanitize))]

//! Multi-player session ownership and output-graph runtime.

pub mod api;
pub mod bridge;
mod error;
mod host;
pub mod owner;
mod rt;
mod session;

#[cfg(target_arch = "wasm32")]
pub mod wasm;

pub use api::{CrossfaderBus, Tap, crossfader_gain};
pub use error::PlayError;
pub use host::{
    Host, HostConfig, HostOwned, HostSettings, HostSettingsChange, HostSettingsControl,
    HostSettingsExec,
};
pub use kithara_play::SessionSampleRate;
pub use rt::{MetronomeConfig, MetronomeConfigChange, MetronomeConfigControl};
pub use session::TransportEvent;
#[cfg(all(
    test,
    feature = "offline",
    feature = "mock",
    not(target_arch = "wasm32")
))]
pub use session::offline::tests::dispatch::mock;
mod consts;

pub use owner::{DeckControl, DeckId, HostCommand, HostCore, HostOwner, HostSettled};

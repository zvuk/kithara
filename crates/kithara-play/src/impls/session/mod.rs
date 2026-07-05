//! Session hosting: cross-platform state + dispatch trait + platform-specific clients.
//!
//! See `crates/kithara-play/CONTEXT.md` "Session Hosting" for the platform
//! asymmetry boundary — this `mod.rs` is the only place in `kithara-play`
//! with `#[cfg(target_arch = ...)]` gates, and they live on the two
//! `mod host_*;` declarations below.

pub mod client;
pub mod state;
#[cfg(test)]
pub(crate) mod testing;

#[cfg(not(target_arch = "wasm32"))]
pub mod host_native;

#[cfg(target_arch = "wasm32")]
pub mod host_web;

pub use client::SessionDispatcher;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use host_native::session_client;
#[cfg(target_arch = "wasm32")]
pub(crate) use host_web::{
    bridge_duration_secs, bridge_is_playing, bridge_position_secs, init_worker_channel,
    session_client, tick_and_poll_remote, warm_up_audio,
};
pub use state::{
    AllocatedSlot, Cmd, CmdMsg, PlayerId, Reply, SessionState, StartStreamFn, run_cmd,
};

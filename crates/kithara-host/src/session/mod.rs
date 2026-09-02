//! Concrete session state, graph dispatch, and platform backends.

mod dispatch;
mod graph;
pub(crate) mod protocol;
pub(crate) mod state;
#[cfg(any(test, feature = "probe"))]
pub mod testing;
mod transport;

#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod native;

#[cfg(target_arch = "wasm32")]
pub(crate) mod web;

pub(crate) use protocol::{
    Cmd, HostCmd, HostDispatcher, HostReply, Reply, SessionError, SessionSampleRate, StreamShape,
};
pub(crate) use state::RootView;
#[cfg(target_arch = "wasm32")]
pub(crate) use web::{
    bridge_duration_secs, bridge_is_playing, bridge_position_secs, remote, tick_and_poll_remote,
    warm_up_audio, worker_channel,
};

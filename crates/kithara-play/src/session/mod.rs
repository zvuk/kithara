//! Session hosting: protocol, state, graph dispatch, and platform clients.

mod dispatch;
mod graph;
pub mod protocol;
pub mod state;
#[cfg(test)]
pub(crate) mod testing;
mod transport;

#[cfg(not(target_arch = "wasm32"))]
pub mod native;

#[cfg(target_arch = "wasm32")]
pub mod web;

pub use dispatch::run_cmd;
pub use protocol::{
    AllocatedSlot, Cmd, CmdMsg, PlayerId, Reply, SessionDispatcher, SessionError, SessionHandle,
    StartStreamFn, TransportPreparationFailure,
};
pub use state::SessionState;
#[cfg(target_arch = "wasm32")]
pub(crate) use web::{
    bridge_duration_secs, bridge_is_playing, bridge_position_secs, local_session, remote_session,
    tick_and_poll_remote, warm_up_audio, worker_channel,
};

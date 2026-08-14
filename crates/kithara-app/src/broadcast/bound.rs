use super::{engine::Backend, state};

/// The phase machine bound to this build's packager.
pub(crate) type Broadcaster = state::Broadcaster<Backend>;
pub(crate) type BroadcastStop = state::BroadcastStop<Backend>;

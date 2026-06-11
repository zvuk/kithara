//! wasm32 backend. Mirrors the facade tree 1:1; cross-platform code is
//! re-imported from `native`.

pub(crate) mod logging;
pub(crate) mod maybe_send;
pub(crate) mod sync;
pub(crate) mod thread;
pub(crate) mod time;

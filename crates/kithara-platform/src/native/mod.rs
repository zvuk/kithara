//! Pure native backend. Zero flash knowledge; cross-platform code lives
//! here and the other backends re-import it.

// parking_lot is declared for non-wasm targets only, so the
// platform-dependent submodules carry the one legal backend cfg.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod env;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod logging;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod maybe_send;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod sync;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod thread;
pub(crate) mod time;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod tokio;

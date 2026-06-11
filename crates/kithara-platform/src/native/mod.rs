//! Pure native backend. Zero flash knowledge; cross-platform code lives
//! here and the other backends re-import it.

// parking_lot is declared for non-wasm targets only, so the
// platform-dependent submodule carries the one legal backend cfg.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod sync;
pub(crate) mod time;

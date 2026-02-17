//! Platform-aware async time utilities.
//!
//! On native: delegates to [`tokio::time::sleep`].
//! On wasm32: no-op (browser fetch has its own retry/timeout mechanisms,
//! and `gloo_timers` is `Rc`-based / `!Send`).

#[cfg(not(target_arch = "wasm32"))]
pub use tokio::time::sleep;

/// No-op sleep for wasm32.
///
/// Browser fetch handles retries/timeouts natively. Avoids `Send` issues
/// with `gloo_timers` (`Rc`-based, not `Send`).
#[cfg(target_arch = "wasm32")]
pub async fn sleep(_duration: std::time::Duration) {}

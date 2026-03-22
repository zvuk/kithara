//! Cross-platform runtime handle.
//!
//! On native, re-exports [`tokio::runtime`] types. On wasm32, provides
//! a lightweight shim so config structs and `try_current()` work without
//! `#[cfg]` gates. The wasm32 handle is a no-op marker — actual async
//! scheduling goes through `tokio_with_wasm`.

#[cfg(not(target_arch = "wasm32"))]
pub use tokio_alias::runtime::*;
#[cfg(not(target_arch = "wasm32"))]
use tokio_with_wasm::alias as tokio_alias;

/// Runtime handle shim for wasm32.
///
/// On wasm32 there is no multi-thread Tokio runtime, but
/// `tokio_with_wasm` provides async scheduling through browser APIs.
/// `try_current()` returns `Ok` so downstream code that stores the
/// handle in `Option<Handle>` gets `Some` and never sees a spurious
/// "no runtime" path.
#[cfg(target_arch = "wasm32")]
#[derive(Clone, Debug)]
pub struct Handle;

#[cfg(target_arch = "wasm32")]
impl Handle {
    /// Returns `Ok(Handle)` — the wasm async runtime is always available.
    pub fn try_current() -> Result<Self, TryCurrentError> {
        Ok(Self)
    }
}

/// Error type kept for API compatibility (never actually returned on wasm32).
#[cfg(target_arch = "wasm32")]
#[derive(Debug)]
pub struct TryCurrentError;

#[cfg(target_arch = "wasm32")]
impl std::fmt::Display for TryCurrentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "no tokio runtime on wasm32")
    }
}

#[cfg(target_arch = "wasm32")]
impl std::error::Error for TryCurrentError {}

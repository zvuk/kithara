/// Runtime handle shim for wasm32.
///
/// On wasm32 there is no multi-thread Tokio runtime, but
/// `tokio_with_wasm` provides async scheduling through browser APIs.
/// `try_current()` returns `Ok` so downstream code that stores the
/// handle in `Option<Handle>` gets `Some` and never sees a spurious
/// "no runtime" path.
#[derive(Clone, Debug)]
pub struct Handle;

impl Handle {
    /// Returns `Ok(Handle)` — the wasm async runtime is always available.
    pub fn try_current() -> Result<Self, TryCurrentError> {
        Ok(Self)
    }
}

/// Error type kept for API compatibility (never actually returned on wasm32).
#[derive(Debug, derive_more::Display)]
#[display("no tokio runtime on wasm32")]
pub struct TryCurrentError;

impl std::error::Error for TryCurrentError {}

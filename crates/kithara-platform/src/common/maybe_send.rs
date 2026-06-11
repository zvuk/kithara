/// Wrapper that unconditionally implements `Send` on WASM.
///
/// On native, `WasmSend<T>` is `Send` only if `T: Send` (no magic).
/// On WASM, `WasmSend<T>` is always `Send`, allowing `!Send` types
/// (like `Writer` wrapping a `JsValue`-backed response stream) to be
/// moved into a Web Worker.
///
/// # Safety contract
///
/// The caller must ensure that:
/// - The wrapped value is **not** actively used across threads simultaneously.
/// - When transferring to a Web Worker, any `!Send` JS-backed values inside
///   must be `None`/uninitialized at transfer time and only populated inside
///   the target worker (where JS objects belong to the local context).
///
/// `FileDownloader` satisfies this: `writer` is `None` when moved to the
/// Worker, and `ensure_writer()` creates the HTTP stream inside the Worker.
///
/// The struct and its accessors are cross-platform; the per-target `Send`
/// impl lives in the owning backend (`native`/`wasm` `maybe_send.rs`).
pub struct WasmSend<T>(T);

impl<T> WasmSend<T> {
    /// Wrap a value.
    pub fn new(value: T) -> Self {
        Self(value)
    }

    /// Immutable access.
    pub fn get(&self) -> &T {
        &self.0
    }

    /// Mutable access.
    pub fn get_mut(&mut self) -> &mut T {
        &mut self.0
    }
}

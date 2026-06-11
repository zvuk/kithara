pub trait MaybeSend: Send {}
impl<T: Send> MaybeSend for T {}

pub trait MaybeSync: Sync {}
impl<T: Sync> MaybeSync for T {}

/// Trait alias for `Future + MaybeSend`.
///
/// On native: `Future + Send` — allows `tokio::spawn`.
/// On WASM: just `Future` — no `Send` requirement.
///
/// Use in return-position impl trait in trait methods:
/// ```ignore
/// fn poll_demand(&mut self) -> impl MaybeSendFuture<Output = Option<Plan>>;
/// ```
pub trait MaybeSendFuture: Future + Send {}
impl<T: Future + Send> MaybeSendFuture for T {}

/// Boxed future that is `Send` on native, unrestricted on WASM.
///
/// Use as return type in trait methods to make the returned future
/// `Send`-compatible on native (enabling `tokio::spawn`) while keeping
/// WASM compatibility.
///
/// ```ignore
/// fn plan(&mut self) -> BoxFuture<'_, PlanOutcome<Self::Plan>>;
/// ```
pub type BoxFuture<'a, T> = std::pin::Pin<Box<dyn Future<Output = T> + Send + 'a>>;

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

// SAFETY: delegates to T's Send impl — no additional invariants.
unsafe impl<T: Send> Send for WasmSend<T> {}

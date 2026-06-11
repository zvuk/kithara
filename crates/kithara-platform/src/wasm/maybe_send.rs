pub trait MaybeSend {}
impl<T> MaybeSend for T {}

pub trait MaybeSync {}
impl<T> MaybeSync for T {}

/// Trait alias for `Future + MaybeSend`.
///
/// On native: `Future + Send` — allows `tokio::spawn`.
/// On WASM: just `Future` — no `Send` requirement.
///
/// Use in return-position impl trait in trait methods:
/// ```ignore
/// fn poll_demand(&mut self) -> impl MaybeSendFuture<Output = Option<Plan>>;
/// ```
pub trait MaybeSendFuture: Future {}
impl<T: Future> MaybeSendFuture for T {}

/// Boxed future that is `Send` on native, unrestricted on WASM.
///
/// Use as return type in trait methods to make the returned future
/// `Send`-compatible on native (enabling `tokio::spawn`) while keeping
/// WASM compatibility.
///
/// ```ignore
/// fn plan(&mut self) -> BoxFuture<'_, PlanOutcome<Self::Plan>>;
/// ```
pub type BoxFuture<'a, T> = std::pin::Pin<Box<dyn Future<Output = T> + 'a>>;

pub use crate::common::maybe_send::WasmSend;

// SAFETY: see the struct-level safety contract on `WasmSend` — the wrapped
// value must not be used across threads simultaneously; `!Send` JS-backed
// values are only populated inside the target Worker.
unsafe impl<T> Send for WasmSend<T> {}

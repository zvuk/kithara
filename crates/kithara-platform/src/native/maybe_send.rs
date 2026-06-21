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

pub use crate::common::maybe_send::WasmSend;

// SAFETY: delegates to T's Send impl — no additional invariants.
unsafe impl<T: Send> Send for WasmSend<T> {}

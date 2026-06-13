//! Platform-aware primitives for native and wasm32 targets.
//!
//! One backend tree per configuration — `native`, `wasm`, or `flash` — is
//! selected by the gated glob re-exports below; the backends mirror one
//! public module tree 1:1 (`sync`, `thread`, `time`, `tokio`, …) and
//! 100% cross-platform code lives in `common`. All crate cfg lives in this
//! file. See the crate `README.md` for per-target backends.

// In the flash-ON lane the inert control surface (`common::flash_inert`) and
// the real-clock arms it aliases compile but are intentionally unwired (the
// engine `flash` module takes the path), so `unreachable_pub`/`dead_code` are
// structurally false-positive there. OFF + wasm lanes re-export the inert
// forms and keep full coverage. See AGENTS.md "Non-Negotiables" legalized
// exceptions.
#[cfg_attr(
    all(not(target_arch = "wasm32"), feature = "flash"),
    expect(unreachable_pub, dead_code)
)]
mod common;

#[cfg(not(target_arch = "wasm32"))]
// Flash-ON wraps (not re-exports) some native items: `unreachable_pub` is
// structurally false-positive there; `dead_code` covers W1-interim unconsumed
// arms and dies with the W2 wrappers; `unused_imports` covers pub-use items in
// native backends that are shadowed by the flash facade arm. OFF lane keeps
// full coverage. See AGENTS.md "Non-Negotiables" legalized exceptions.
#[cfg_attr(feature = "flash", expect(unreachable_pub, dead_code, unused_imports))]
mod native;
#[cfg(all(not(target_arch = "wasm32"), not(feature = "flash")))]
pub use native::*;

#[cfg(target_arch = "wasm32")]
mod wasm;
#[cfg(target_arch = "wasm32")]
pub use wasm::*;

#[cfg(all(not(target_arch = "wasm32"), feature = "flash"))]
pub mod flash;

// W3 propagate-down cancel — the workspace's only cancel surface (the legacy
// runtime-backed roots were dropped in 3.4).
pub use common::cancel::{CancelGroup, CancelScope, CancelToken, CancelWakerGuard, Cancelled};
// `kithara_platform::flash::*` (macro emissions, prod attributes) must resolve
// in every configuration: inert forms off the engine.
#[cfg(not(all(not(target_arch = "wasm32"), feature = "flash")))]
pub use common::flash_inert as flash;
#[cfg(all(not(target_arch = "wasm32"), feature = "flash"))]
pub use flash::*;

// Root item re-exports: kept until the W5 mass name migration; they resolve
// through the gated backend globs above.
#[cfg(not(target_arch = "wasm32"))]
pub use self::env::mutation_lock as env_mutation_lock;
pub use self::{
    logging::log_error,
    maybe_send::{BoxFuture, MaybeSend, MaybeSendFuture, MaybeSync, WasmSend},
    sync::{Condvar, Mutex, MutexGuard, NotAvailable, RwLock, RwLockReadGuard, RwLockWriteGuard},
    thread::{Duration, JoinHandle, Thread, ThreadId, current, park, park_timeout, sleep, spawn},
};

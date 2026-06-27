//! Flash-only sync-primitive diagnostics: a runtime registry of every live
//! primitive (Mutex/RwLock + the engine-backed kinds) with provenance — where
//! it was created, who holds it, who waits on it — so a hung flash test dumps
//! the full wait-for picture instead of an opaque counter. Compiled ONLY under
//! `flash` (gated in `lib.rs`), so it is free in production.
//!
//! Gated at runtime by `KITHARA_FLASH_SYNC_TRACE` (default OFF): off ⇒
//! [`register`] returns `None` and a wrapped primitive pays only a null check.
//! `KITHARA_FLASH_SYNC_BT=1` adds the dumping thread's backtrace. See the crate
//! `CONTEXT.md` "Virtual time (`flash`)".

mod registry;
mod thread;
mod toggle;

pub(in crate::flash) use registry::{PrimEntry, PrimKind, register, snapshot};
pub(in crate::flash) use thread::current_thread_context;
pub(in crate::flash) use toggle::{bt_enabled, trace_enabled};

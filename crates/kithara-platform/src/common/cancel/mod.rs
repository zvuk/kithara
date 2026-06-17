//! Propagate-down cancellation: a `std`-only `Node` tree (`AtomicBool` flag +
//! `Weak` children + waker slots) and the public `CancelToken` / `CancelGroup` /
//! `CancelScope` types built on it.
//!
//! Built entirely on `std::sync` (no `parking_lot`, no async-runtime cancel
//! crate), so it compiles unchanged on `wasm32`. `cancel()` flips this node's flag, drains its
//! own wakers, then recurses **down** through the `Weak` children; reads
//! (`is_cancelled`) are a single `Acquire` load of one flag. The full contract —
//! the parent-liveness invariant, the born-cancelled race, and the
//! `Release`/`Acquire` ordering — lives in `crates/kithara-play/README.md`
//! "Cancel Hierarchy" section and in `AGENTS.md`.
//!
//! `std::sync::Mutex` poisoning: every guarded section is a linear structural
//! edit (insert/remove a slot, snapshot the children). A panic can only escape
//! while running a consumer waker, which is a consumer bug, and the structure
//! stays consistent regardless. So a poisoned lock is recovered with
//! `into_inner()` rather than propagated — `unwrap()`/`expect()` are forbidden in
//! production code, and there is no correct state to lose.
//!
//! Flash: the bare `std::sync::Mutex` is deliberate and conformant. The platform
//! treats `Mutex`/`RwLock` as **flash-blind by contract** (see `flash/sync/mod.rs`)
//! — they guard bounded critical sections whose holder always runs (never parks
//! while held), so virtual time can never advance with the lock held and a waiter
//! is always served; flash adds no semantics and `flash::sync::Mutex` is a plain
//! re-export of the native one. Flash-awareness lives only in the **wait**
//! primitives (`Condvar`/`Notify`/`mpsc`). Cancel owns no wait: every section here
//! is bounded and wakers fire outside the lock; `on_cancel` merely delivers a wake
//! into a consumer's flash-aware `Condvar` (e.g. `kithara-storage` `wait_range`),
//! where the actual blocking wait and its flash mediation live. So there is no
//! flash variant to swap in and nothing to make generic over the lock type.

mod group;
mod node;
mod scope;
mod token;
mod wait;

pub use group::CancelGroup;
pub use scope::CancelScope;
pub use token::{CancelToken, CancelWakerGuard};
pub use wait::Cancelled;

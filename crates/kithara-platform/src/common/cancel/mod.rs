//! Propagate-down cancellation: a `std`-only `Node` tree (`AtomicBool` flag +
//! `Weak` children + waker slots) and the public `CancelRoot` / `CancelToken` /
//! `CancelGroup` / `CancelScope` types built on it.
//!
//! Built entirely on `std::sync` (no `parking_lot`, no `tokio_util`), so it
//! compiles unchanged on `wasm32`. `cancel()` flips this node's flag, drains its
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

mod group;
mod node;
mod scope;
mod token;
mod wait;

// `CancelGroup` (group.rs) is built and tested here but NOT re-exported at the
// crate root in 3.2: the root `CancelGroup` name still resolves to the legacy
// `cancel_group::CancelGroup`, and the switch to this one is the atomic
// construction-site migration in 3.3.
pub use scope::CancelScope;
pub use token::{CancelRoot, CancelToken, CancelWakerGuard};
pub use wait::{Cancelled, CancelledOwned};

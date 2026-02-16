//! Conditional `Send`/`Sync` bounds for wasm32 compatibility.
//!
//! On native targets, `MaybeSend` = `Send` and `MaybeSync` = `Sync`.
//! On wasm32, both are auto-implemented for all types (no-op).
//!
//! Use these in trait bounds to avoid duplicating entire trait definitions
//! with `#[cfg]` gates. Does NOT work in `dyn` position — only auto-traits
//! (`Send`, `Sync`, `Unpin`) can appear after `dyn Trait +`.

#[cfg(not(target_arch = "wasm32"))]
pub trait MaybeSend: Send {}
#[cfg(not(target_arch = "wasm32"))]
impl<T: Send> MaybeSend for T {}

#[cfg(target_arch = "wasm32")]
pub trait MaybeSend {}
#[cfg(target_arch = "wasm32")]
impl<T> MaybeSend for T {}

#[cfg(not(target_arch = "wasm32"))]
pub trait MaybeSync: Sync {}
#[cfg(not(target_arch = "wasm32"))]
impl<T: Sync> MaybeSync for T {}

#[cfg(target_arch = "wasm32")]
pub trait MaybeSync {}
#[cfg(target_arch = "wasm32")]
impl<T> MaybeSync for T {}

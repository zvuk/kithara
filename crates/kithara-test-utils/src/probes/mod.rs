//! Runtime support for the `#[kithara::probe]` attribute macro.
//!
//! Provides:
//! - [`IntoProbeArg`] — trait that domain types implement so the macro
//!   can blindly convert any annotated argument to the wire `u64` USDT
//!   takes.
//! - [`register_probes`] — idempotent global USDT registration.
//!
//! # Wire format
//!
//! All probe args are encoded as `u64`. Domain enums use stable
//! discriminants (see individual `IntoProbeArg` impls). `Option<T>`
//! uses `u64::MAX` as the `None` sentinel — reserve that value if your
//! `T` could legitimately produce it.
//!
//! # Activation
//!
//! `#[kithara::probe]` emits no-op code unless the *consumer crate*
//! (the one carrying the annotated function) sets either `cfg(test)`
//! or its own `feature = "test-utils"`. This module's own
//! `usdt-probes` feature toggles whether [`register_probes`] is a real
//! call or a no-op stub.

#![cfg_attr(target_arch = "wasm32", allow(unused_imports))]

mod usdt_wire;
mod wire;

pub use usdt_wire::{fire_0, fire_1, fire_2, fire_3, fire_4, fire_5, fire_6};
#[cfg(not(target_arch = "wasm32"))]
pub use wire::OWNED_INSTALL_ID;
pub use wire::{
    IntoProbeArg, Probe, bump_install_id, caller_fn_above, current_install_id, current_thread_u64,
    next_probe_seq, next_thread_probe_seq, register_probes,
};

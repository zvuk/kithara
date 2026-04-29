//! Runtime support for the `#[kithara::probe]` attribute macro.
//!
//! Provides:
//! - [`IntoProbeArg`] — trait that domain types implement so the macro
//!   can blindly convert any annotated argument to the wire `u64` USDT
//!   takes.
//! - [`register_probes`] — idempotent global USDT registration.
//! - [`kithara`] — namespace re-export so consumers write
//!   `use kithara_probes::kithara;` and `#[kithara::probe]`.
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
//! (the one carrying the annotated function) declares a feature named
//! `usdt-probes` and it is enabled at build time. This crate's own
//! `usdt-probes` feature toggles whether [`register_probes`] is a real
//! call or a no-op stub.

#![cfg_attr(target_arch = "wasm32", allow(unused_imports))]

pub mod kithara;
mod wire;

pub use kithara_probe_macros::{Probe, probe};
pub use wire::{IntoProbeArg, Probe, register_probes};

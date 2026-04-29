//! `#[kithara::probe]` attribute macro.
//!
//! Wraps a function with USDT probe firing + paired `tracing::event!`
//! emission. Provider name and tracing target are derived from the
//! consumer crate's `CARGO_PKG_NAME` (with `-` → `_` normalization), so
//! a function annotated inside `kithara-stream` becomes a
//! `kithara_stream:<fn_name>` USDT probe and a
//! `target = "kithara_stream_probe"` tracing event.
//!
//! The macro emits three independent layers:
//!
//! 1. **Function body** — never gated. Original code runs on every
//!    target / feature combination.
//! 2. **USDT block** — gated by
//!    `cfg(all(feature = "usdt-probes", not(target_arch = "wasm32")))`.
//!    `usdt::provider!` requires inline asm, so wasm32 cannot link it.
//! 3. **`tracing::event!`** — gated by `cfg(feature = "usdt-probes")`
//!    only. Works on every architecture, including wasm32, so
//!    `probe_capture` integration tests stay observable everywhere.
//!
//! Each annotated function must declare arguments whose types implement
//! `kithara_probes::IntoProbeArg`. Probe-function names must be unique
//! per crate — USDT addresses probes as `provider:probe`, and a name
//! collision causes a linker symbol conflict.

mod expand;

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use syn::{DeriveInput, ItemFn, parse_macro_input};

#[proc_macro_attribute]
pub fn probe(attr: TokenStream, item: TokenStream) -> TokenStream {
    let attr2 = TokenStream2::from(attr);
    let filter = match expand::parse_filter(attr2) {
        Ok(f) => f,
        Err(e) => return e.into_compile_error().into(),
    };
    let input = parse_macro_input!(item as ItemFn);
    expand::expand(&input, filter)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Derive `kithara_probes::Probe` for a struct.
///
/// Each field whose type implements `kithara_probes::IntoProbeArg`
/// becomes a u64 slot in the generated USDT probe and a named field in
/// the paired `tracing::event!`. Fields can be skipped with
/// `#[probe(skip)]` and renamed in the wire format with
/// `#[probe(name = "alias")]`.
///
/// Generated impl exposes [`Probe::record_probe`] that fires the probe
/// at the call site with a caller-supplied `name` (used as the
/// dynamic `probe = "..."` field on the tracing event). The USDT
/// probe name is fixed at derive expansion time to the struct name in
/// `snake_case`, which keeps dtrace addressing stable across calls.
#[proc_macro_derive(Probe, attributes(probe))]
pub fn derive_probe(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand::expand_derive(&input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

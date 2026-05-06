//! Safe USDT-emit wrappers for `#[kithara::probe]`.
//!
//! All `unsafe`-bearing inline asm lives in this single module. The
//! macro-generated code at the consumer site only ever issues a
//! `kithara_probes::fire_N(...)` function call, so consumer crates can
//! keep `#![forbid(unsafe_code)]` and still publish probes.
//!
//! USDT addressing trade-off: every per-arity entry point lands under
//! the shared `kithara` provider (`kithara:::probe_<N>`). Per-callsite
//! distinguishability is preserved through the `name` argument, which
//! is also the `probe = "..."` field on the paired `tracing::event!` —
//! `probe_capture` and other tracing consumers see the original probe
//! name unchanged. `DTrace` consumers must filter on the probe name
//! field rather than the static probe symbol; the trade-off buys
//! consumers freedom from carrying USDT inline asm across `#![forbid]`
//! boundaries.
//!
//! `usdt` v0.6 caps a single provider's probe at 6 wire arguments;
//! `fire_N` mirrors that ceiling. Probes wanting >6 fields must
//! decompose or fold via `#[derive(Probe)]` on a struct.
//!
//! ## Lint suppressions
//!
//! This file owns two structural lint carve-outs (carved out of
//! `rust.no-lint-suppression` in `.config/ast-grep/`):
//!
//! - `#[expect(unsafe_code, ...)]` on the `prov` module — the
//!   `usdt::provider!` proc-macro expands to inline asm. Consumer crates
//!   never see this `unsafe`; it is contained here.
//! - `#[expect(clippy::items_after_statements, ...)]` on each
//!   `fire_<N>` body — the USDT probe macro expands to a per-call
//!   `static` item interleaved with `let _: u64 = ...` statements,
//!   which clippy flags. The macro is external (`usdt` crate); the
//!   shape cannot be reordered.
//!
//! See `crates/kithara-probes/README.md` for the carve-out contract.

#[cfg(all(feature = "usdt-probes", not(target_arch = "wasm32")))]
#[expect(
    unsafe_code,
    reason = "USDT inline asm in `usdt::provider!` expansion — single-point carve-out (see module docs)."
)]
#[::usdt::provider(provider = "kithara")]
mod prov {
    pub(super) fn probe_0() {}
    pub(super) fn probe_1(_a0: u64) {}
    pub(super) fn probe_2(_a0: u64, _a1: u64) {}
    pub(super) fn probe_3(_a0: u64, _a1: u64, _a2: u64) {}
    pub(super) fn probe_4(_a0: u64, _a1: u64, _a2: u64, _a3: u64) {}
    pub(super) fn probe_5(_a0: u64, _a1: u64, _a2: u64, _a3: u64, _a4: u64) {}
    pub(super) fn probe_6(_a0: u64, _a1: u64, _a2: u64, _a3: u64, _a4: u64, _a5: u64) {}
}

#[expect(
    clippy::items_after_statements,
    reason = "USDT probe macro mixes per-call `static` items with `let _: u64 = ...` arg-bindings; the shape is fixed by the external `usdt` crate."
)]
pub fn fire_0(name: &'static str) {
    let _ = name;
    #[cfg(all(feature = "usdt-probes", not(target_arch = "wasm32")))]
    prov::probe_0!(|| ());
}

#[expect(
    clippy::items_after_statements,
    reason = "USDT probe macro mixes per-call `static` items with `let _: u64 = ...` arg-bindings; the shape is fixed by the external `usdt` crate."
)]
pub fn fire_1(name: &'static str, a0: u64) {
    let _ = (name, a0);
    #[cfg(all(feature = "usdt-probes", not(target_arch = "wasm32")))]
    prov::probe_1!(|| a0);
}

#[expect(
    clippy::items_after_statements,
    reason = "USDT probe macro mixes per-call `static` items with `let _: u64 = ...` arg-bindings; the shape is fixed by the external `usdt` crate."
)]
pub fn fire_2(name: &'static str, a0: u64, a1: u64) {
    let _ = (name, a0, a1);
    #[cfg(all(feature = "usdt-probes", not(target_arch = "wasm32")))]
    prov::probe_2!(|| (a0, a1));
}

#[expect(
    clippy::items_after_statements,
    reason = "USDT probe macro mixes per-call `static` items with `let _: u64 = ...` arg-bindings; the shape is fixed by the external `usdt` crate."
)]
pub fn fire_3(name: &'static str, a0: u64, a1: u64, a2: u64) {
    let _ = (name, a0, a1, a2);
    #[cfg(all(feature = "usdt-probes", not(target_arch = "wasm32")))]
    prov::probe_3!(|| (a0, a1, a2));
}

#[expect(
    clippy::items_after_statements,
    reason = "USDT probe macro mixes per-call `static` items with `let _: u64 = ...` arg-bindings; the shape is fixed by the external `usdt` crate."
)]
pub fn fire_4(name: &'static str, a0: u64, a1: u64, a2: u64, a3: u64) {
    let _ = (name, a0, a1, a2, a3);
    #[cfg(all(feature = "usdt-probes", not(target_arch = "wasm32")))]
    prov::probe_4!(|| (a0, a1, a2, a3));
}

#[expect(
    clippy::items_after_statements,
    reason = "USDT probe macro mixes per-call `static` items with `let _: u64 = ...` arg-bindings; the shape is fixed by the external `usdt` crate."
)]
pub fn fire_5(name: &'static str, a0: u64, a1: u64, a2: u64, a3: u64, a4: u64) {
    let _ = (name, a0, a1, a2, a3, a4);
    #[cfg(all(feature = "usdt-probes", not(target_arch = "wasm32")))]
    prov::probe_5!(|| (a0, a1, a2, a3, a4));
}

#[expect(
    clippy::items_after_statements,
    reason = "USDT probe macro mixes per-call `static` items with `let _: u64 = ...` arg-bindings; the shape is fixed by the external `usdt` crate."
)]
pub fn fire_6(name: &'static str, a0: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64) {
    let _ = (name, a0, a1, a2, a3, a4, a5);
    #[cfg(all(feature = "usdt-probes", not(target_arch = "wasm32")))]
    prov::probe_6!(|| (a0, a1, a2, a3, a4, a5));
}

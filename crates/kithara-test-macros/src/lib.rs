//! `kithara-test-macros` — proc-macros для kithara test infrastructure.
//!
//! `lib.rs` содержит только обязательные `#[proc_macro*]` entry-points
//! (Rust требует их в crate root) и делегирует логику в per-macro модули:
//! - [`test`] — `#[kithara::test]` (sync/async/native/wasm с case+fixture).
//! - [`fixture`] — `#[kithara::fixture]` (rstest-fixture replacement).
//! - [`probe`] — `#[kithara::probe]` + `#[derive(kithara::Probe)]`
//!   (USDT + tracing instrumentation; auto-gated
//!   `cfg(any(test, feature = "probe"))`, with the emit wrapped in a
//!   `kithara_test_utils::rtsan::permit` guard so probes stay active but
//!   `RTSan`-transparent under `--cfg rtsan`).
//! - [`mock`] — `#[kithara::mock]` (unimock forwarder, gated `cfg(any(test, feature = "mock"))`).

mod fixture;
mod hang_watchdog;
mod mock;
mod probe;
mod rtsan;
mod test;

use proc_macro::TokenStream;

/// `#[kithara::test]` — unified sync/async/native/wasm test attribute.
/// См. [`test`] для синтаксиса аргументов.
#[proc_macro_attribute]
pub fn test(attr: TokenStream, item: TokenStream) -> TokenStream {
    test::expand(attr, item)
}

/// `#[kithara::hang_watchdog]` — wraps a function with a `HangDetector`.
/// Generated code refers to `::kithara_test_utils::hang::*`.
#[proc_macro_attribute]
pub fn hang_watchdog(attr: TokenStream, item: TokenStream) -> TokenStream {
    hang_watchdog::expand(attr, item)
}

/// `#[kithara::fixture]` — rstest-fixture replacement.
#[proc_macro_attribute]
pub fn fixture(attr: TokenStream, item: TokenStream) -> TokenStream {
    fixture::expand(attr, item)
}

/// `#[kithara::probe]` — USDT + tracing-event instrumentation.
/// Тело гейтится `cfg(any(test, feature = "probe"))` → no-op в проде; emit
/// обёрнут в `rtsan::permit`, поэтому под `--cfg rtsan` пробы активны, но
/// `RTSan` их не флагает.
#[proc_macro_attribute]
pub fn probe(attr: TokenStream, item: TokenStream) -> TokenStream {
    probe::expand_attr(attr, item)
}

/// `#[kithara::mock]` — workspace replacement for `#[unimock(...)]`.
/// Гейтится `cfg(any(test, feature = "mock"))` → trait-декларация
/// в проде остаётся без mock-impl.
#[proc_macro_attribute]
pub fn mock(args: TokenStream, item: TokenStream) -> TokenStream {
    mock::expand(args, item)
}

/// `#[kithara::rtsan_forbid_blocking]` — forbid blocking in this function and
/// its callees by marking it a `RealtimeSanitizer`-checked entry point. Expands
/// to `#[cfg_attr(rtsan, sanitize(realtime = "nonblocking"))]`; off `rtsan` the
/// function is byte-identical.
#[proc_macro_attribute]
pub fn rtsan_forbid_blocking(_attr: TokenStream, item: TokenStream) -> TokenStream {
    rtsan::expand_forbid_blocking(item)
}

/// `#[kithara::rtsan_allow_blocking]` — allow a genuinely-blocking function
/// inside a `forbid_blocking` context by wrapping its body in a reentrant
/// `kithara_test_utils::rtsan::permit` guard (`__rtsan_disable`/`enable`).
/// Off `rtsan` the guard is a zero-cost no-op.
#[proc_macro_attribute]
pub fn rtsan_allow_blocking(_attr: TokenStream, item: TokenStream) -> TokenStream {
    rtsan::expand_allow_blocking(item)
}

/// `#[derive(kithara::Probe)]` — generates `record_probe()` для value-type probes.
/// Тело гейтится `cfg(any(test, feature = "probe"))`; emit обёрнут в
/// `rtsan::permit` (активно, но `RTSan`-прозрачно под `--cfg rtsan`).
#[proc_macro_derive(Probe, attributes(probe))]
pub fn derive_probe(input: TokenStream) -> TokenStream {
    probe::expand_derive_entry(input)
}

/// `#[derive(kithara::IntoProbeArg)]` — generates round-trippable
/// `IntoProbeArg` impl for a `Copy` newtype struct (single field, named
/// or tuple). Saves repeating the trivial `self.0 as u64` /
/// `Self(packed as Inner)` boilerplate across every domain id type that
/// participates in probes. Multi-field structs are rejected — they
/// must provide an explicit packed impl with a documented layout.
#[proc_macro_derive(IntoProbeArg)]
pub fn derive_into_probe_arg(input: TokenStream) -> TokenStream {
    probe::expand_derive_into_probe_arg_entry(input)
}

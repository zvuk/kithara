//! `kithara-test-macros` — proc-macros для kithara test infrastructure.
//!
//! `lib.rs` содержит только обязательные `#[proc_macro*]` entry-points
//! (Rust требует их в crate root) и делегирует логику в per-macro модули:
//! - [`test_macro`] — `#[kithara::test]` (sync/async/native/wasm с case+fixture).
//! - [`fixture_macro`] — `#[kithara::fixture]` (rstest-fixture replacement).
//! - [`probe_macro`] — `#[kithara::probe]` + `#[derive(kithara::Probe)]`
//!   (USDT + tracing instrumentation; auto-gated `cfg(any(test, feature = "test-utils"))`).
//! - [`mock_macro`] — `#[kithara::mock]` (unimock forwarder, auto-gated).

mod fixture_macro;
mod mock_macro;
mod probe_macro;
mod test_macro;

use proc_macro::TokenStream;

/// `#[kithara::test]` — unified sync/async/native/wasm test attribute.
/// См. [`test_macro`] для синтаксиса аргументов.
#[proc_macro_attribute]
pub fn test(attr: TokenStream, item: TokenStream) -> TokenStream {
    test_macro::expand(attr, item)
}

/// `#[kithara::fixture]` — rstest-fixture replacement.
#[proc_macro_attribute]
pub fn fixture(attr: TokenStream, item: TokenStream) -> TokenStream {
    fixture_macro::expand(attr, item)
}

/// `#[kithara::probe]` — USDT + tracing-event instrumentation.
/// Тело гейтится `cfg(any(test, feature = "test-utils"))` → no-op в проде.
#[proc_macro_attribute]
pub fn probe(attr: TokenStream, item: TokenStream) -> TokenStream {
    probe_macro::expand_attr(attr, item)
}

/// `#[kithara::mock]` — workspace replacement for `#[unimock(...)]`.
/// Гейтится `cfg(any(test, feature = "test-utils"))` → trait-декларация
/// в проде остаётся без mock-impl.
#[proc_macro_attribute]
pub fn mock(args: TokenStream, item: TokenStream) -> TokenStream {
    mock_macro::expand(args, item)
}

/// `#[derive(kithara::Probe)]` — generates `record_probe()` для value-type probes.
/// Тело гейтится `cfg(any(test, feature = "test-utils"))`.
#[proc_macro_derive(Probe, attributes(probe))]
pub fn derive_probe(input: TokenStream) -> TokenStream {
    probe_macro::expand_derive_entry(input)
}

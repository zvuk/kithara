use proc_macro::TokenStream;
use quote::quote;
use syn::{Ident, ItemFn, LitBool, parse_macro_input, parse_quote};

/// Argument of `#[kithara::flash(..)]`: a dynamic-flash mode flip, or a
/// real-I/O bracket.
enum Mode {
    /// `flash` / `flash(true)` / `flash(false)` — dynamic flash guard.
    Dynamic(bool),
    /// `flash(io)` — the fn body is ONE real I/O operation in flight
    /// ([`kithara_platform::time::real_io`]).
    Io,
}

/// Expand `#[kithara::flash]` / `#[kithara::flash(true|false)]` /
/// `#[kithara::flash(io)]`.
///
/// `true`/`false` wrap a PROD fn so its body propagates flash dynamically: a
/// sync fn gets an RAII guard ([`kithara_platform::time::enter_dynamic`]); an
/// async fn gets a per-poll combinator
/// ([`kithara_platform::time::flash_dynamic`]) that survives `.await` (the
/// mode is a thread-local, so it must be re-asserted on every poll). `on`
/// defaults to `true`; `#[kithara::flash(false)]` carves a REAL region inside
/// an otherwise-flash callstack.
///
/// `io` brackets the body as ONE real I/O operation
/// ([`kithara_platform::time::real_io`]): while it is in flight the virtual
/// clock is paced to real time. Unlike the dynamic modes this is GLOBAL
/// engine state, not a thread-local, so one plain RAII guard suffices for
/// sync and async alike — in an async fn it lives in the future's state
/// across `.await`s and drops on completion, cancellation, or unwind. Do NOT
/// place it on a method rewritten by `#[async_trait]`: the rewrite turns the
/// method into a sync constructor returning a boxed future, so the guard
/// would drop before the I/O even starts — annotate the inherent async fn it
/// delegates to instead.
///
/// Off the `flash-time` feature all three are no-ops, so the attribute
/// compiles away to the bare body.
pub(crate) fn expand(attr: TokenStream, item: TokenStream) -> TokenStream {
    let mode = if attr.is_empty() {
        Mode::Dynamic(true)
    } else if let Ok(b) = syn::parse::<LitBool>(attr.clone()) {
        Mode::Dynamic(b.value)
    } else {
        let ident = parse_macro_input!(attr as Ident);
        if ident == "io" {
            Mode::Io
        } else {
            return syn::Error::new_spanned(ident, "expected `true`, `false` or `io`")
                .to_compile_error()
                .into();
        }
    };
    let mut f = parse_macro_input!(item as ItemFn);
    let block = &f.block;
    *f.block = match mode {
        Mode::Dynamic(on) if f.sig.asyncness.is_some() => parse_quote!({
            ::kithara_platform::time::flash_dynamic(#on, async move #block).await
        }),
        Mode::Dynamic(on) => parse_quote!({
            let __flash_guard = ::kithara_platform::time::enter_dynamic(#on);
            #block
        }),
        Mode::Io => parse_quote!({
            let __real_io_guard = ::kithara_platform::time::real_io();
            #block
        }),
    };
    quote!(#f).into()
}

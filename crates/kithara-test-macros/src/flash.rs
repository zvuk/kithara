use proc_macro::TokenStream;
use quote::quote;
use syn::{ItemFn, LitBool, parse_macro_input, parse_quote};

/// Expand `#[kithara::flash]` / `#[kithara::flash(true|false)]`.
///
/// Wraps a PROD fn so its body propagates flash dynamically: a sync fn gets an
/// RAII guard ([`kithara_platform::time::enter_dynamic`]); an async fn gets a
/// per-poll combinator ([`kithara_platform::time::flash_dynamic`]) that survives
/// `.await`. Off the `flash-time` feature both `enter_dynamic` and `flash_dynamic`
/// are no-ops, so the attribute compiles away to the bare body.
///
/// `on` defaults to `true`; `#[kithara::flash(false)]` carves a REAL region inside
/// an otherwise-flash callstack.
pub(crate) fn expand(attr: TokenStream, item: TokenStream) -> TokenStream {
    let on = if attr.is_empty() {
        true
    } else {
        parse_macro_input!(attr as LitBool).value
    };
    let mut f = parse_macro_input!(item as ItemFn);
    if f.sig.asyncness.is_some() {
        let block = &f.block;
        *f.block = parse_quote!({
            ::kithara_platform::time::flash_dynamic(#on, async move #block).await
        });
    } else {
        let block = &f.block;
        *f.block = parse_quote!({
            let __flash_guard = ::kithara_platform::time::enter_dynamic(#on);
            #block
        });
    }
    quote!(#f).into()
}

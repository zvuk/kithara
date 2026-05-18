use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use syn::{DeriveInput, Error, ItemFn, parse_macro_input};

use super::{
    derive::{expand_derive, expand_derive_into_probe_arg},
    expand::expand,
    parse::parse_filter,
};

/// Entry-point for `#[kithara::probe]` — forwarded from `lib.rs`.
pub(crate) fn expand_attr(attr: TokenStream, item: TokenStream) -> TokenStream {
    let attr2 = TokenStream2::from(attr);
    let filter = match parse_filter(attr2) {
        Ok(f) => f,
        Err(e) => return e.into_compile_error().into(),
    };
    let input = parse_macro_input!(item as ItemFn);
    expand(&input, filter)
        .unwrap_or_else(Error::into_compile_error)
        .into()
}

/// Entry-point for `#[derive(kithara::Probe)]` — forwarded from `lib.rs`.
pub(crate) fn expand_derive_entry(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand_derive(&input)
        .unwrap_or_else(Error::into_compile_error)
        .into()
}

/// Entry-point for `#[derive(kithara::IntoProbeArg)]` — forwarded from `lib.rs`.
pub(crate) fn expand_derive_into_probe_arg_entry(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand_derive_into_probe_arg(&input)
        .unwrap_or_else(Error::into_compile_error)
        .into()
}

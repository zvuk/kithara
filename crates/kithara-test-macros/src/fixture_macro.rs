//! Expansion logic for `#[kithara::fixture]` (rstest-fixture replacement).
//!
//! A zero-arg fixture passes through unchanged. A fixture with parameters is
//! transformed to zero-arg by binding each parameter to a same-named function
//! call (or `name`-without-`_` prefix for ignored fixtures), so the body keeps
//! seeing the parameter as before.

use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{FnArg, ItemFn, Pat, parse_macro_input};

pub(crate) fn expand(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let func = parse_macro_input!(item as ItemFn);

    if func.sig.inputs.is_empty() {
        return quote! { #func }.into();
    }

    let vis = &func.vis;
    let fn_name = &func.sig.ident;
    let ret_type = &func.sig.output;
    let body_stmts = &func.block.stmts;
    let asyncness = func.sig.asyncness;
    let remaining_attrs: Vec<_> = func.attrs.iter().collect();

    let dep_bindings: Vec<_> = func
        .sig
        .inputs
        .iter()
        .filter_map(|arg| {
            let FnArg::Typed(pt) = arg else { return None };
            let Pat::Ident(pi) = &*pt.pat else {
                return None;
            };
            let name = &pi.ident;
            let ty = &pt.ty;
            let mutability = &pi.mutability;
            let dep_fn = {
                let s = name.to_string();
                let trimmed = s.trim_start_matches('_');
                if trimmed.is_empty() {
                    name.clone()
                } else {
                    format_ident!("{}", trimmed)
                }
            };
            Some(quote! { let #mutability #name: #ty = #dep_fn(); })
        })
        .collect();

    quote! {
        #(#remaining_attrs)*
        #vis #asyncness fn #fn_name() #ret_type {
            #(#dep_bindings)*
            #(#body_stmts)*
        }
    }
    .into()
}

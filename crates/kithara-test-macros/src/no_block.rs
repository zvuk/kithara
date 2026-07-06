use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{Expr, ExprLit, ItemFn, Lit, MetaNameValue, parse_macro_input, parse_quote};

const DEFAULT_BUDGET_MS: u64 = 25;

pub(crate) fn expand(attr: TokenStream, item: TokenStream) -> TokenStream {
    expand_with_path(attr, item, &quote!(::kithara_test_utils::no_block))
}

pub(crate) fn expand_facade(attr: TokenStream, item: TokenStream) -> TokenStream {
    expand_with_path(attr, item, &quote!(::kithara::no_block))
}

pub(crate) fn expand_allow_block(attr: TokenStream, item: TokenStream) -> TokenStream {
    expand_allow_block_with_path(attr, item, &quote!(::kithara_test_utils::no_block))
}

pub(crate) fn expand_allow_block_facade(attr: TokenStream, item: TokenStream) -> TokenStream {
    expand_allow_block_with_path(attr, item, &quote!(::kithara::no_block))
}

fn expand_with_path(attr: TokenStream, item: TokenStream, path: &TokenStream2) -> TokenStream {
    let budget_ms = if attr.is_empty() {
        DEFAULT_BUDGET_MS
    } else {
        match parse_budget(attr) {
            Ok(v) => v,
            Err(e) => return e.to_compile_error().into(),
        }
    };
    let mut f = parse_macro_input!(item as ItemFn);
    if f.sig.asyncness.is_none() {
        return syn::Error::new_spanned(
            f.sig.fn_token,
            "#[kithara::no_block] supports async fns only; for sync blocking escapes use #[kithara::allow_block]",
        )
        .to_compile_error()
        .into();
    }
    let name = f.sig.ident.to_string();
    let block = &f.block;
    *f.block = parse_quote!({
        #path::watch(
            concat!(module_path!(), "::", #name),
            #budget_ms,
            async move #block,
        )
        .await
    });
    quote!(#f).into()
}

fn expand_allow_block_with_path(
    attr: TokenStream,
    item: TokenStream,
    path: &TokenStream2,
) -> TokenStream {
    if !attr.is_empty() {
        return syn::Error::new_spanned(
            proc_macro2::TokenStream::from(attr),
            "#[kithara::allow_block] takes no arguments",
        )
        .to_compile_error()
        .into();
    }
    let input = parse_macro_input!(item as ItemFn);
    let attrs = &input.attrs;
    let vis = &input.vis;
    let sig = &input.sig;
    let block = &input.block;
    if sig.asyncness.is_some() {
        quote! {
            #(#attrs)*
            #vis #sig {
                #path::permit_poll(async move #block).await
            }
        }
        .into()
    } else {
        quote! {
            #(#attrs)*
            #vis #sig {
                let __no_block_permit = #path::permit();
                #block
            }
        }
        .into()
    }
}

fn parse_budget(attr: TokenStream) -> syn::Result<u64> {
    let meta = syn::parse::<MetaNameValue>(attr)?;
    if !meta.path.is_ident("budget_ms") {
        return Err(syn::Error::new_spanned(
            meta.path,
            "expected `budget_ms = <int>`",
        ));
    }
    match meta.value {
        Expr::Lit(ExprLit {
            lit: Lit::Int(lit), ..
        }) => lit.base10_parse::<u64>(),
        value => Err(syn::Error::new_spanned(value, "expected integer literal")),
    }
}

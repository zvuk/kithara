#![allow(
    clippy::option_if_let_else,
    reason = "match is more readable for format_ident! case names"
)]

use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{Attribute, Expr, FnArg, Ident, ItemFn, Pat, Token};

pub(crate) enum ParamKind {
    Case,
    Future,
    Fixture,
}

pub(crate) struct ParamInfo {
    pub(crate) ty: Box<syn::Type>,
    pub(crate) name: Ident,
    pub(crate) mutability: Option<Token![mut]>,
    pub(crate) kind: ParamKind,
}

fn has_attr(attrs: &[Attribute], name: &str) -> bool {
    attrs
        .iter()
        .any(|a| a.path().segments.first().is_some_and(|s| s.ident == name))
}

pub(crate) fn extract_params(func: &ItemFn) -> Vec<ParamInfo> {
    func.sig
        .inputs
        .iter()
        .filter_map(|arg| {
            let FnArg::Typed(pt) = arg else { return None };
            let Pat::Ident(pi) = &*pt.pat else {
                return None;
            };
            let kind = if has_attr(&pt.attrs, "case") {
                ParamKind::Case
            } else if has_attr(&pt.attrs, "future") {
                ParamKind::Future
            } else {
                ParamKind::Fixture
            };
            Some(ParamInfo {
                kind,
                name: pi.ident.clone(),
                ty: pt.ty.clone(),
                mutability: pi.mutability,
            })
        })
        .collect()
}

pub(crate) fn make_preamble(params: &[ParamInfo], case_values: Option<&[Expr]>) -> TokenStream2 {
    let mut stmts = Vec::new();
    let mut case_idx = 0;

    for p in params {
        let name = &p.name;
        let ty = &p.ty;
        let fn_name = {
            let s = name.to_string();
            let trimmed = s.trim_start_matches('_');
            if trimmed.is_empty() {
                name.clone()
            } else {
                format_ident!("{}", trimmed)
            }
        };
        let mutability = &p.mutability;
        match p.kind {
            ParamKind::Case => {
                if let Some(vals) = case_values
                    && let Some(val) = vals.get(case_idx)
                {
                    stmts.push(quote! { let #mutability #name: #ty = #val; });
                    case_idx += 1;
                }
            }
            ParamKind::Future => {
                stmts.push(quote! { let #mutability #name = #fn_name(); });
            }
            ParamKind::Fixture => {
                stmts.push(quote! { let #mutability #name: #ty = #fn_name(); });
            }
        }
    }

    quote! { #(#stmts)* }
}

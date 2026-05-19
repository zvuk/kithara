#![allow(
    clippy::option_if_let_else,
    reason = "match is more readable for format_ident! case names"
)]

use quote::format_ident;
use syn::{Attribute, Expr, Ident, Meta, Token, parse::Parser, punctuated::Punctuated};

pub(crate) struct Case {
    pub(crate) name: Option<Ident>,
    pub(crate) values: Vec<Expr>,
}

pub(crate) fn is_case_attr(attr: &Attribute) -> bool {
    attr.path()
        .segments
        .first()
        .is_some_and(|s| s.ident == "case")
}

pub(crate) fn extract_cases(attrs: &[Attribute]) -> syn::Result<Vec<Case>> {
    let mut cases = Vec::new();
    for attr in attrs.iter().filter(|a| is_case_attr(a)) {
        let path = attr.path();
        let name = if path.segments.len() > 1 {
            path.segments.last().map(|s| s.ident.clone())
        } else {
            None
        };
        let values = if let Meta::List(list) = &attr.meta {
            Punctuated::<Expr, Token![,]>::parse_terminated
                .parse2(list.tokens.clone())?
                .into_iter()
                .collect()
        } else {
            vec![]
        };
        cases.push(Case { name, values });
    }
    Ok(cases)
}

/// Case-name convention: `{fn_name}_{case.name}` or `{fn_name}_case_{i+1}`.
pub(crate) fn case_ident(fn_name: &Ident, case: &Case, index: usize) -> Ident {
    match &case.name {
        Some(name) => format_ident!("{}_{}", fn_name, name),
        None => format_ident!("{}_case_{}", fn_name, index + 1),
    }
}

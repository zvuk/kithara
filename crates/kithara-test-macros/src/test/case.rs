#![allow(
    clippy::option_if_let_else,
    reason = "match is more readable for format_ident! case names"
)]

use kithara_test_args::TestPlatform;
use quote::format_ident;
use syn::{Attribute, Error, Expr, Ident, Meta, Token, parse::Parser, punctuated::Punctuated};

pub(crate) struct Case {
    pub(crate) name: Option<Ident>,
    /// Declaration position; numbers an unnamed case.
    pub(crate) index: usize,
    /// The platform a leading `platform = <name>` argument declares.
    pub(crate) platform: Option<TestPlatform>,
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
    for (index, attr) in attrs.iter().filter(|a| is_case_attr(a)).enumerate() {
        let path = attr.path();
        let name = if path.segments.len() > 1 {
            path.segments.last().map(|s| s.ident.clone())
        } else {
            None
        };
        let mut values: Vec<Expr> = if let Meta::List(list) = &attr.meta {
            Punctuated::<Expr, Token![,]>::parse_terminated
                .parse2(list.tokens.clone())?
                .into_iter()
                .collect()
        } else {
            vec![]
        };
        let platform = match values.first().map(declared_platform).transpose()? {
            Some(Some(platform)) => {
                values.remove(0);
                Some(platform)
            }
            _ => None,
        };
        cases.push(Case {
            name,
            index,
            platform,
            values,
        });
    }
    Ok(cases)
}

/// Reads `platform = <name>`; any other expression is an ordinary value.
fn declared_platform(value: &Expr) -> syn::Result<Option<TestPlatform>> {
    let Expr::Assign(assign) = value else {
        return Ok(None);
    };
    let Expr::Path(key) = &*assign.left else {
        return Ok(None);
    };
    if !key.path.is_ident("platform") {
        return Ok(None);
    }
    let Expr::Path(name) = &*assign.right else {
        return Err(Error::new_spanned(
            &assign.right,
            "`platform` takes a platform name",
        ));
    };
    let name = name
        .path
        .get_ident()
        .ok_or_else(|| Error::new_spanned(&assign.right, "`platform` takes a platform name"))?;
    name.to_string()
        .parse()
        .map(Some)
        .map_err(|message| Error::new(name.span(), message))
}

/// Case-name convention: `{fn_name}_{case.name}` or `{fn_name}_case_{index+1}`.
pub(crate) fn case_ident(fn_name: &Ident, case: &Case) -> Ident {
    match &case.name {
        Some(name) => format_ident!("{}_{}", fn_name, name),
        None => format_ident!("{}_case_{}", fn_name, case.index + 1),
    }
}

use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{Attribute, Error, Expr, FnArg, Ident, ItemFn, Meta, Pat, Token, ext::IdentExt};

pub(crate) enum ParamKind {
    Case,
    Future,
    Await,
    Fixture,
    BorrowedFixture,
}

#[derive(Clone)]
pub(crate) struct Preamble {
    pub(crate) body: TokenStream2,
    owners: Option<(TokenStream2, TokenStream2)>,
}

impl Preamble {
    pub(crate) fn with_body(&self, statements: &[syn::Stmt]) -> Self {
        let mut preamble = self.clone();
        preamble.body.extend(quote! { #(#statements)* });
        preamble
    }

    pub(crate) fn scoped(&self, enter_runtime: bool) -> TokenStream2 {
        let Some((bindings, values)) = &self.owners else {
            return TokenStream2::new();
        };
        let runtime = enter_runtime.then(|| {
            quote! { let _kithara_fixture_runtime = __rt.enter(); }
        });
        quote! {
            #runtime
            let #bindings = #values;
        }
    }
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

pub(crate) fn extract_params(func: &ItemFn) -> syn::Result<Vec<ParamInfo>> {
    func.sig
        .inputs
        .iter()
        .filter_map(|arg| {
            let FnArg::Typed(pt) = arg else { return None };
            let Pat::Ident(pi) = &*pt.pat else {
                return None;
            };
            let kind = if let Some(attr) = pt.attrs.iter().find(|attr| attr.path().is_ident("by_ref")) {
                if !matches!(attr.meta, Meta::Path(_)) {
                    return Some(Err(Error::new_spanned(attr, "expected #[by_ref]")));
                }
                if has_attr(&pt.attrs, "case") || has_attr(&pt.attrs, "future") {
                    return Some(Err(Error::new_spanned(
                        attr,
                        "#[by_ref] requires a synchronous fixture, not a case or future",
                    )));
                }
                if !matches!(&*pt.ty, syn::Type::Reference(reference) if reference.mutability.is_none()) {
                    return Some(Err(Error::new_spanned(
                        &pt.ty,
                        "#[by_ref] requires a shared reference type",
                    )));
                }
                ParamKind::BorrowedFixture
            } else if has_attr(&pt.attrs, "case") {
                ParamKind::Case
            } else if let Some(attr) = pt.attrs.iter().find(|attr| attr.path().is_ident("future")) {
                match &attr.meta {
                    Meta::Path(_) => ParamKind::Future,
                    Meta::List(_)
                        if attr.parse_args::<Ident>().is_ok_and(|value| value == "awt") =>
                    {
                        ParamKind::Await
                    }
                    _ => {
                        return Some(Err(Error::new_spanned(
                            attr,
                            "expected #[future] or #[future(awt)]",
                        )));
                    }
                }
            } else {
                ParamKind::Fixture
            };
            Some(Ok(ParamInfo {
                kind,
                name: pi.ident.clone(),
                ty: pt.ty.clone(),
                mutability: pi.mutability,
            }))
        })
        .collect()
}

pub(crate) fn make_preamble(params: &[ParamInfo], case_values: Option<&[Expr]>) -> Preamble {
    let mut stmts = Vec::new();
    let mut case_idx = 0;
    let mut owner_names = Vec::new();
    let mut owner_types = Vec::new();
    let mut owner_values = Vec::new();

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
            ParamKind::Await => {
                stmts.push(quote! { let #mutability #name: #ty = #fn_name().await; });
            }
            ParamKind::Future => {
                stmts.push(quote! { let #mutability #name = #fn_name(); });
            }
            ParamKind::Fixture => {
                stmts.push(quote! { let #mutability #name: #ty = #fn_name(); });
            }
            ParamKind::BorrowedFixture => {
                let syn::Type::Reference(reference) = &**ty else {
                    unreachable!("borrowed fixture types are validated during extraction");
                };
                let owner = format_ident!("__kithara_fixture_{}", name.unraw());
                let owned_type = &reference.elem;
                owner_names.push(quote! { #owner });
                owner_types.push(quote! { #owned_type });
                owner_values.push(quote! { #fn_name() });
                stmts.push(quote! { let #mutability #name: #ty = &#owner; });
            }
        }
    }

    let owners = (!owner_names.is_empty()).then(|| {
        (
            quote! { (#(#owner_names,)*): (#(#owner_types,)*) },
            quote! { (#(#owner_values,)*) },
        )
    });
    Preamble {
        body: quote! { #(#stmts)* },
        owners,
    }
}

#[cfg(test)]
mod tests {
    use super::{extract_params, make_preamble};

    #[test]
    fn awaited_fixture_is_ready_before_the_test_body() -> syn::Result<()> {
        let function = syn::parse_str("async fn test(#[future(awt)] media: Vec<u8>) {}")?;
        let params = extract_params(&function)?;
        let preamble = make_preamble(&params, None).body.to_string();
        assert!(preamble.contains("media () . await"), "{preamble}");
        Ok(())
    }
}

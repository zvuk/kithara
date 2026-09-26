use proc_macro2::TokenStream;
use quote::quote;
use syn::{Error, Expr, Lit, Meta, Path, Result, Token, punctuated::Punctuated};

pub(super) fn collect(
    attribute: &Meta,
    added: &mut Vec<TokenStream>,
    deserialize: &mut Option<Path>,
) -> Result<()> {
    let Meta::List(list) = attribute else {
        added.push(quote! { #attribute });
        return Ok(());
    };
    if !list.path.is_ident("serde") {
        added.push(quote! { #attribute });
        return Ok(());
    }
    let arguments = list.parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)?;
    let mut kept: Vec<Meta> = Vec::new();
    for argument in arguments {
        let with = argument.path().is_ident("with");
        if !with && !argument.path().is_ident("deserialize_with") {
            kept.push(argument);
            continue;
        }
        let Meta::NameValue(value) = &argument else {
            return Err(Error::new_spanned(
                argument,
                "a serde deserializer requires a string path",
            ));
        };
        let Expr::Lit(expression) = &value.value else {
            return Err(Error::new_spanned(
                argument,
                "a serde deserializer requires a string path",
            ));
        };
        let Lit::Str(literal) = &expression.lit else {
            return Err(Error::new_spanned(
                argument,
                "a serde deserializer requires a string path",
            ));
        };
        let mut path = literal.value();
        if with {
            path.push_str("::deserialize");
        }
        set(deserialize, syn::parse_str(&path)?)?;
    }
    if !kept.is_empty() {
        added.push(quote! { serde(#(#kept),*) });
    }
    Ok(())
}

pub(super) fn set(destination: &mut Option<Path>, path: Path) -> Result<()> {
    if destination.is_some() {
        return Err(Error::new_spanned(
            path,
            "select one patch field deserializer",
        ));
    }
    *destination = Some(path);
    Ok(())
}

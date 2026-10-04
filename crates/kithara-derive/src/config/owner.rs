use proc_macro2::TokenStream;
use quote::quote;
use syn::{
    Data, DeriveInput, Ident, Token, Type, parenthesized,
    parse::{Parse, ParseStream},
    punctuated::Punctuated,
    token,
};

/// Where the owner's configuration lives: stored at a field path, or owned by
/// the one field it delegates to.
struct OwnerSpec {
    config: Option<Type>,
    path: Punctuated<Ident, Token![.]>,
    delegate: bool,
}

impl Parse for OwnerSpec {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        if input.peek(Ident) && input.peek2(token::Paren) {
            let keyword: Ident = input.parse()?;
            if keyword != "delegate" {
                return Err(syn::Error::new_spanned(
                    keyword,
                    "expected `delegate(field)`",
                ));
            }
            let content;
            parenthesized!(content in input);
            let field: Ident = content.parse()?;
            if !content.is_empty() || !input.is_empty() {
                return Err(syn::Error::new_spanned(
                    field,
                    "`delegate` takes exactly one field",
                ));
            }
            let mut path = Punctuated::new();
            path.push_value(field);
            return Ok(Self {
                path,
                config: None,
                delegate: true,
            });
        }
        let config: Type = input.parse()?;
        let (config, path) = if input.is_empty() {
            let Type::Path(path) = config else {
                return Err(input.error("expected a named field"));
            };
            let Some(field) = path.path.get_ident() else {
                return Err(input.error("a nested field path needs an explicit config type"));
            };
            let mut fields = Punctuated::new();
            fields.push_value(field.clone());
            (None, fields)
        } else {
            input.parse::<Token![,]>()?;
            (Some(config), Punctuated::parse_separated_nonempty(input)?)
        };
        if !input.is_empty() {
            return Err(input.error("expected a field path after the configuration type"));
        }
        Ok(Self {
            config,
            path,
            delegate: false,
        })
    }
}

pub(crate) fn expand(input: TokenStream) -> syn::Result<TokenStream> {
    let input: DeriveInput = syn::parse2(input)?;
    let Data::Struct(ref data) = input.data else {
        return Err(syn::Error::new_spanned(
            &input.ident,
            "ConfigOwner can only be derived for a struct",
        ));
    };
    let Some(spec_attr) = input
        .attrs
        .iter()
        .find(|attr| attr.path().is_ident("config_owner"))
    else {
        return Err(syn::Error::new_spanned(
            &input.ident,
            "expected #[config_owner(field)], #[config_owner(ConfigType, field.path)] or #[config_owner(delegate(field))]",
        ));
    };
    let spec: OwnerSpec = spec_attr.parse_args()?;
    let Some(first) = spec.path.first() else {
        return Err(syn::Error::new_spanned(
            spec_attr,
            "configuration field path must not be empty",
        ));
    };
    let syn::Fields::Named(fields) = &data.fields else {
        return Err(syn::Error::new_spanned(
            &input.ident,
            "ConfigOwner requires named fields",
        ));
    };
    let Some(field) = fields
        .named
        .iter()
        .find(|field| field.ident.as_ref() == Some(first))
    else {
        return Err(syn::Error::new_spanned(
            first,
            "configuration path must start with a field of this struct",
        ));
    };
    let name = &input.ident;
    let ty = &field.ty;
    let (config, read, write) = if spec.delegate {
        (
            quote!(<#ty as ::kithara_config::ConfigOwner>::Config),
            quote!(::kithara_config::ConfigOwner::config(&self.#first)),
            quote!(::kithara_config::ConfigOwnerMut::config_mut(&mut self.#first)),
        )
    } else {
        let config = spec.config.as_ref().unwrap_or(ty);
        let (read, write) = (spec.path.iter(), spec.path.iter());
        (
            quote!(#config),
            quote!(&self.#(#read).*),
            quote!(&mut self.#(#write).*),
        )
    };
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();
    let mutable: Vec<_> = input
        .attrs
        .iter()
        .filter(|attr| attr.path().is_ident("config_owner_mut"))
        .collect();
    if mutable.len() > 1 {
        return Err(syn::Error::new_spanned(
            mutable[1],
            "duplicate config_owner_mut attribute",
        ));
    }
    let mutable_impl = mutable.first().map(|attribute| {
        if !matches!(&attribute.meta, syn::Meta::Path(_)) {
            return Err(syn::Error::new_spanned(
                attribute,
                "config_owner_mut takes no arguments",
            ));
        }
        Ok(quote! {
            impl #impl_generics ::kithara_config::ConfigOwnerMut for #name #ty_generics #where_clause {
                fn config_mut(&mut self) -> &mut Self::Config {
                    #write
                }
            }
        })
    }).transpose()?;
    Ok(quote! {
        impl #impl_generics ::kithara_config::ConfigOwner for #name #ty_generics #where_clause {
            type Config = #config;

            fn config(&self) -> &Self::Config {
                #read
            }
        }
        #mutable_impl
    })
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;
    use quote::quote;

    use super::expand;

    #[kithara::test(native)]
    fn delegate_names_exactly_one_field_of_the_struct() {
        for (attribute, message) in [
            (
                quote!(#[config_owner(delegated(inner))]),
                "expected `delegate(field)`",
            ),
            (
                quote!(#[config_owner(delegate(inner, other))]),
                "`delegate` takes exactly one field",
            ),
            (
                quote!(#[config_owner(delegate(missing))]),
                "configuration path must start with a field of this struct",
            ),
        ] {
            let input = quote!(#attribute struct Owner { inner: Inner, other: Inner });
            let refusal = expand(input).expect_err("the owner is refused");
            assert_eq!(refusal.to_string(), message);
        }
    }
}

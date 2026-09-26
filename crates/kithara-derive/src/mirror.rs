#![cfg(feature = "mirror")]

use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::{format_ident, quote};
use syn::{
    Attribute, Data, DataEnum, DataStruct, DeriveInput, Error, Fields, GenericParam, Generics,
    Ident, Index, Lifetime, LifetimeParam, Path,
};

#[derive(Default)]
struct Options {
    from: Option<Path>,
    from_ref: Option<Path>,
    into: Option<Path>,
}

#[derive(Default)]
struct MemberOptions {
    rename: Option<Ident>,
    as_ref: bool,
    copy: bool,
    skip: bool,
    tuple: bool,
}

pub(crate) fn expand(input: TokenStream) -> TokenStream {
    expand_inner(&syn::parse_macro_input!(input as DeriveInput))
        .unwrap_or_else(Error::into_compile_error)
        .into()
}

fn expand_inner(input: &DeriveInput) -> syn::Result<TokenStream2> {
    let options = parse_options(&input.attrs)?;
    if options.from.is_none() && options.from_ref.is_none() && options.into.is_none() {
        return Err(Error::new_spanned(
            &input.ident,
            "Mirror requires `from`, `from_ref`, or `into`",
        ));
    }

    let mut output = TokenStream2::new();
    if let Some(source) = options.from {
        output.extend(expand_from(input, &source, false)?);
    }
    if let Some(source) = options.from_ref {
        output.extend(expand_from(input, &source, true)?);
    }
    if let Some(target) = options.into {
        output.extend(expand_into(input, &target)?);
    }
    Ok(output)
}

fn parse_options(attrs: &[Attribute]) -> syn::Result<Options> {
    let mut options = Options::default();
    for attr in attrs.iter().filter(|attr| attr.path().is_ident("mirror")) {
        attr.parse_nested_meta(|meta| {
            let value = meta.value()?;
            let path: Path = value.parse()?;
            if meta.path.is_ident("from") {
                options.from = Some(path);
            } else if meta.path.is_ident("from_ref") {
                options.from_ref = Some(path);
            } else if meta.path.is_ident("into") {
                options.into = Some(path);
            } else {
                return Err(meta.error("expected `from`, `from_ref`, or `into`"));
            }
            Ok(())
        })?;
    }
    Ok(options)
}

fn member_options(attrs: &[Attribute]) -> syn::Result<MemberOptions> {
    let mut options = MemberOptions::default();
    for attr in attrs.iter().filter(|attr| attr.path().is_ident("mirror")) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("rename") {
                options.rename = Some(meta.value()?.parse()?);
            } else if meta.path.is_ident("copy") {
                options.copy = true;
            } else if meta.path.is_ident("as_ref") {
                options.as_ref = true;
            } else if meta.path.is_ident("skip") {
                options.skip = true;
            } else if meta.path.is_ident("tuple") {
                options.tuple = true;
            } else {
                return Err(meta.error("expected `rename`, `copy`, `as_ref`, `skip`, or `tuple`"));
            }
            Ok(())
        })?;
    }
    if options.copy && options.as_ref {
        return Err(Error::new_spanned(
            attrs.first(),
            "a mirror field cannot be both `copy` and `as_ref`",
        ));
    }
    Ok(options)
}

fn expand_from(input: &DeriveInput, source: &Path, by_ref: bool) -> syn::Result<TokenStream2> {
    let name = &input.ident;
    let target_generics = &input.generics;
    let (_, ty_generics, where_clause) = target_generics.split_for_impl();
    let (impl_generics, reference) = impl_generics_for_ref(target_generics, by_ref);
    let (impl_generics, _, _) = impl_generics.split_for_impl();
    let source_ty = if by_ref {
        quote!(&#reference #source)
    } else {
        quote!(#source)
    };
    let body = match &input.data {
        Data::Struct(data) => from_struct(data, source, by_ref)?,
        Data::Enum(data) => from_enum(data, source, by_ref)?,
        Data::Union(union) => {
            return Err(Error::new_spanned(
                union.union_token,
                "Mirror does not support unions",
            ));
        }
    };
    Ok(quote! {
        impl #impl_generics ::core::convert::From<#source_ty> for #name #ty_generics #where_clause {
            fn from(value: #source_ty) -> Self { #body }
        }
    })
}

fn impl_generics_for_ref(generics: &Generics, by_ref: bool) -> (Generics, TokenStream2) {
    if !by_ref {
        return (generics.clone(), TokenStream2::new());
    }
    if let Some(lifetime) = generics.lifetimes().next() {
        let lifetime = &lifetime.lifetime;
        return (generics.clone(), quote!(#lifetime));
    }
    let mut generics = generics.clone();
    let lifetime = Lifetime::new("'mirror", Span::call_site());
    generics.params.insert(
        0,
        GenericParam::Lifetime(LifetimeParam::new(lifetime.clone())),
    );
    (generics, quote!(#lifetime))
}

fn from_struct(data: &DataStruct, _source: &Path, by_ref: bool) -> syn::Result<TokenStream2> {
    match &data.fields {
        Fields::Named(fields) => {
            let values = fields
                .named
                .iter()
                .map(|field| {
                    let target = field.ident.as_ref().expect("named field");
                    let options = member_options(&field.attrs)?;
                    let source = options.rename.clone().unwrap_or_else(|| target.clone());
                    let value = field_value(quote!(value.#source), &options, by_ref);
                    Ok(quote!(#target: #value))
                })
                .collect::<syn::Result<Vec<_>>>()?;
            Ok(quote!(Self { #(#values,)* }))
        }
        Fields::Unnamed(fields) => {
            let values = fields
                .unnamed
                .iter()
                .enumerate()
                .map(|(index, field)| {
                    let index = Index::from(index);
                    let options = member_options(&field.attrs)?;
                    Ok(field_value(quote!(value.#index), &options, by_ref))
                })
                .collect::<syn::Result<Vec<_>>>()?;
            Ok(quote!(Self(#(#values),*)))
        }
        Fields::Unit => Ok(quote!(Self)),
    }
}

fn from_enum(data: &DataEnum, source: &Path, by_ref: bool) -> syn::Result<TokenStream2> {
    let arms = data
        .variants
        .iter()
        .filter_map(|variant| {
            let target = &variant.ident;
            let variant_options = match member_options(&variant.attrs) {
                Ok(options) => options,
                Err(error) => return Some(Err(error)),
            };
            if variant_options.skip {
                return None;
            }
            let source_variant = variant_options.rename.unwrap_or_else(|| target.clone());
            let result = enum_arm_fields(
                &variant.fields,
                source,
                &source_variant,
                target,
                by_ref,
                variant_options.tuple,
            )
            .map(|(pattern, value)| quote!(#pattern => #value));
            Some(result)
        })
        .collect::<syn::Result<Vec<_>>>()?;
    Ok(quote!(match value { #(#arms,)* }))
}

fn enum_arm_fields(
    fields: &Fields,
    source: &Path,
    source_variant: &Ident,
    target_variant: &Ident,
    by_ref: bool,
    source_tuple: bool,
) -> syn::Result<(TokenStream2, TokenStream2)> {
    match fields {
        Fields::Unit => Ok((
            quote!(#source::#source_variant),
            quote!(Self::#target_variant),
        )),
        Fields::Unnamed(fields) => {
            let bindings: Vec<_> = (0..fields.unnamed.len())
                .map(|index| format_ident!("field_{index}"))
                .collect();
            let values = fields
                .unnamed
                .iter()
                .zip(&bindings)
                .map(|(field, binding)| {
                    let options = member_options(&field.attrs)?;
                    Ok(field_value(quote!(#binding), &options, by_ref))
                })
                .collect::<syn::Result<Vec<_>>>()?;
            Ok((
                quote!(#source::#source_variant(#(#bindings),*)),
                quote!(Self::#target_variant(#(#values),*)),
            ))
        }
        Fields::Named(fields) => {
            let members = fields
                .named
                .iter()
                .map(|field| {
                    let target = field.ident.as_ref().expect("named field");
                    let options = member_options(&field.attrs)?;
                    let source = options.rename.clone().unwrap_or_else(|| target.clone());
                    let binding = format_ident!("mirror_{target}");
                    let value = field_value(quote!(#binding), &options, by_ref);
                    Ok((source, binding, target.clone(), value))
                })
                .collect::<syn::Result<Vec<_>>>()?;
            let named_patterns = members
                .iter()
                .map(|(source, binding, _, _)| quote!(#source: #binding));
            let tuple_patterns = members.iter().map(|(_, binding, _, _)| binding);
            let values = members
                .iter()
                .map(|(_, _, target, value)| quote!(#target: #value));
            let pattern = if source_tuple {
                quote!(#source::#source_variant(#(#tuple_patterns),*))
            } else {
                quote!(#source::#source_variant { #(#named_patterns,)* })
            };
            Ok((pattern, quote!(Self::#target_variant { #(#values,)* })))
        }
    }
}

fn field_value(value: TokenStream2, options: &MemberOptions, by_ref: bool) -> TokenStream2 {
    if by_ref && options.copy {
        quote!(*#value)
    } else if by_ref && options.as_ref {
        quote!(#value.as_ref())
    } else {
        value
    }
}

fn expand_into(input: &DeriveInput, target: &Path) -> syn::Result<TokenStream2> {
    let name = &input.ident;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();
    let body = match &input.data {
        Data::Struct(data) => into_struct(data, target)?,
        Data::Enum(data) => into_enum(data, name, target)?,
        Data::Union(union) => {
            return Err(Error::new_spanned(
                union.union_token,
                "Mirror does not support unions",
            ));
        }
    };
    Ok(quote! {
        impl #impl_generics ::core::convert::From<#name #ty_generics> for #target #where_clause {
            fn from(value: #name #ty_generics) -> Self { #body }
        }
    })
}

fn into_struct(data: &DataStruct, target: &Path) -> syn::Result<TokenStream2> {
    match &data.fields {
        Fields::Named(fields) => {
            let values = fields
                .named
                .iter()
                .filter_map(|field| {
                    let source = field.ident.as_ref().expect("named field");
                    let options = match member_options(&field.attrs) {
                        Ok(options) => options,
                        Err(error) => return Some(Err(error)),
                    };
                    if options.skip {
                        return None;
                    }
                    let target = options.rename.unwrap_or_else(|| source.clone());
                    Some(Ok(quote!(#target: value.#source)))
                })
                .collect::<syn::Result<Vec<_>>>()?;
            Ok(quote!(#target { #(#values,)* }))
        }
        Fields::Unnamed(fields) => {
            let values = (0..fields.unnamed.len()).map(Index::from);
            Ok(quote!(#target(#(value.#values),*)))
        }
        Fields::Unit => Ok(quote!(#target)),
    }
}

fn into_enum(data: &DataEnum, source_type: &Ident, target: &Path) -> syn::Result<TokenStream2> {
    let arms = data
        .variants
        .iter()
        .map(|variant| {
            let source = &variant.ident;
            let options = member_options(&variant.attrs)?;
            let target_variant = options.rename.unwrap_or_else(|| source.clone());
            let (pattern, value) = into_enum_arm(
                &variant.fields,
                source_type,
                source,
                target,
                &target_variant,
            )?;
            Ok(quote!(#pattern => #value))
        })
        .collect::<syn::Result<Vec<_>>>()?;
    Ok(quote!(match value { #(#arms,)* }))
}

fn into_enum_arm(
    fields: &Fields,
    source_type: &Ident,
    source_variant: &Ident,
    target: &Path,
    target_variant: &Ident,
) -> syn::Result<(TokenStream2, TokenStream2)> {
    match fields {
        Fields::Unit => Ok((
            quote!(#source_type::#source_variant),
            quote!(#target::#target_variant),
        )),
        Fields::Unnamed(fields) => {
            let bindings: Vec<_> = (0..fields.unnamed.len())
                .map(|index| format_ident!("field_{index}"))
                .collect();
            Ok((
                quote!(#source_type::#source_variant(#(#bindings),*)),
                quote!(#target::#target_variant(#(#bindings),*)),
            ))
        }
        Fields::Named(fields) => {
            let members = fields
                .named
                .iter()
                .map(|field| {
                    let source = field.ident.as_ref().expect("named field");
                    let options = member_options(&field.attrs)?;
                    let target = options.rename.unwrap_or_else(|| source.clone());
                    let binding = format_ident!("mirror_{source}");
                    Ok((quote!(#source: #binding), quote!(#target: #binding)))
                })
                .collect::<syn::Result<Vec<_>>>()?;
            let patterns = members.iter().map(|(pattern, _)| pattern);
            let values = members.iter().map(|(_, value)| value);
            Ok((
                quote!(#source_type::#source_variant { #(#patterns,)* }),
                quote!(#target::#target_variant { #(#values,)* }),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::expand_inner;

    #[kithara::test(native, flash(false))]
    fn refuses_a_union() {
        let input = syn::parse_quote!(
            #[mirror(from = Source)]
            union Value { byte: u8 }
        );
        assert!(expand_inner(&input).is_err());
    }

    #[kithara::test(native, flash(false))]
    fn refuses_conflicting_reference_operations() {
        let input = syn::parse_quote!(
            #[mirror(from_ref = Source)]
            struct Value {
                #[mirror(copy, as_ref)]
                value: u8,
            }
        );
        assert!(expand_inner(&input).is_err());
    }

    #[kithara::test(native, flash(false))]
    fn renames_an_into_variant() {
        let input = syn::parse_quote!(
            #[mirror(into = Target)]
            enum Value {
                #[mirror(rename = Renamed)]
                Local,
            }
        );
        let output = expand_inner(&input).expect("supported mirror").to_string();
        assert!(output.contains("Target :: Renamed"), "{output}");
    }
}

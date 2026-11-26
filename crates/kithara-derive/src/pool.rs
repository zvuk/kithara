//! Expansion of `#[derive(Pool)]`.

#![cfg(feature = "pool")]

use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::quote;
use syn::{
    Attribute, Data, DeriveInput, Error, Fields, GenericArgument, Ident, PathArguments, Result,
    Type, Visibility, ext::IdentExt, parse_macro_input, spanned::Spanned,
};

/// Names the generated builder already owns; a field with one would shadow it.
const RESERVED: [&str; 4] = ["builder", "build", "__build_region", "overall_budget"];

struct Slot<'a> {
    docs: Vec<&'a Attribute>,
    field: &'a Ident,
    key: &'a Type,
}

pub(crate) fn expand(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    derive(&input)
        .unwrap_or_else(Error::into_compile_error)
        .into()
}

fn derive(input: &DeriveInput) -> Result<TokenStream2> {
    if !input.generics.params.is_empty() || input.generics.where_clause.is_some() {
        return Err(Error::new_spanned(
            &input.generics,
            "`Pool` schemas must be concrete: the generic parameter belongs to `PoolSlot<K>`",
        ));
    }
    let Data::Struct(data) = &input.data else {
        return Err(Error::new_spanned(
            &input.ident,
            "`Pool` derives only on a struct with named `PoolSlot<K>` fields",
        ));
    };
    let Fields::Named(fields) = &data.fields else {
        return Err(Error::new_spanned(
            &input.ident,
            "`Pool` derives only on a struct with named `PoolSlot<K>` fields",
        ));
    };
    if fields.named.is_empty() {
        return Err(Error::new_spanned(
            &input.ident,
            "`Pool` schema must declare at least one `PoolSlot<K>` field",
        ));
    }
    let slots = fields
        .named
        .iter()
        .map(|field| {
            let ident = field
                .ident
                .as_ref()
                .ok_or_else(|| Error::new_spanned(field, "`Pool` schema fields must be named"))?;
            if !matches!(field.vis, Visibility::Inherited) {
                return Err(Error::new_spanned(
                    &field.vis,
                    format!("pool slot `{ident}` must be private to the schema"),
                ));
            }
            let unraw = ident.unraw().to_string();
            if RESERVED.contains(&unraw.as_str()) {
                return Err(Error::new_spanned(
                    ident,
                    format!("pool slot `{unraw}` collides with a generated builder name"),
                ));
            }
            Ok(Slot {
                docs: field
                    .attrs
                    .iter()
                    .filter(|attr| attr.path().is_ident("doc"))
                    .collect(),
                field: ident,
                key: slot_key(&field.ty)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    let name = &input.ident;
    let context = Ident::new("context", Span::mixed_site());
    let fields = slots.iter().map(|slot| slot.field).collect::<Vec<_>>();
    let keys = slots.iter().map(|slot| slot.key).collect::<Vec<_>>();
    let params = slots
        .iter()
        .map(|Slot { docs, field, .. }| quote!(#(#docs)* #field: ::kithara_bufpool::PoolConfig));

    Ok(quote! {
        #[::kithara_bufpool::__private::bon::bon(crate = ::kithara_bufpool::__private::bon)]
        impl #name {
            #[builder(
                builder_type(vis = ""),
                start_fn(name = builder, vis = ""),
                finish_fn(name = build, vis = "")
            )]
            #[doc(hidden)]
            fn __build_region(
                #[builder(start_fn)]
                overall_budget: ::kithara_bufpool::OverallBudget,
                #(#params,)*
            ) -> ::core::result::Result<
                ::kithara_bufpool::PoolRegion<Self>,
                ::kithara_bufpool::PoolError,
            > {
                ::kithara_bufpool::PoolRegion::__build(overall_budget, |#context| {
                    ::core::result::Result::Ok(Self {
                        #(#fields: #context.slot::<#keys>(#fields)?,)*
                    })
                })
            }
        }

        #(
            impl ::kithara_bufpool::HasPool<#keys> for #name {
                fn __slot(&self) -> &::kithara_bufpool::PoolSlot<#keys> {
                    &self.#fields
                }
            }
        )*
    })
}

/// Return `K` from a field written as a path ending in `PoolSlot<K>`.
fn slot_key(ty: &Type) -> Result<&Type> {
    let refuse = || {
        Error::new(
            ty.span(),
            "pool schema fields must have type `PoolSlot<K>` with one key type",
        )
    };
    let Type::Path(path) = ty else {
        return Err(refuse());
    };
    let segment = path.path.segments.last().ok_or_else(refuse)?;
    if path.qself.is_some() || segment.ident != "PoolSlot" {
        return Err(refuse());
    }
    let PathArguments::AngleBracketed(arguments) = &segment.arguments else {
        return Err(refuse());
    };
    match arguments.args.iter().collect::<Vec<_>>().as_slice() {
        [GenericArgument::Type(key)] => Ok(key),
        _ => Err(refuse()),
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;
    use syn::{DeriveInput, parse_quote};

    use super::derive;

    fn expand(input: &DeriveInput) -> String {
        let output = derive(input).expect("valid schema expands");
        syn::parse2::<syn::File>(output.clone()).expect("expansion parses as Rust");
        output.to_string()
    }

    fn refusal(input: &DeriveInput) -> String {
        derive(input).expect_err("schema is refused").to_string()
    }

    #[kithara::test(native)]
    fn expands_one_builder_and_one_capability_per_slot() {
        let output = expand(&parse_quote! {
            /// Pools.
            pub struct AppPools {
                /// Byte pool.
                bytes: PoolSlot<u8>,
                samples: PoolSlot<f32>,
            }
        });
        assert_eq!(output.matches("PoolRegion :: __build (").count(), 1);
        assert!(output.contains("impl :: kithara_bufpool :: HasPool < u8 > for AppPools"));
        assert!(output.contains("impl :: kithara_bufpool :: HasPool < f32 > for AppPools"));
        assert!(
            output.contains("# [doc = r\" Byte pool.\"] bytes : :: kithara_bufpool :: PoolConfig")
        );
        assert!(output.contains("bytes : context . slot :: < u8 > (bytes) ?"));
    }

    #[kithara::test(native)]
    fn accepts_qualified_slot_paths_and_raw_identifiers() {
        let output = expand(&parse_quote! {
            struct Pools {
                r#type: kithara_bufpool::PoolSlot<PoolAlias<Tag, VecKey<u8, 1>>>,
                text: ::kithara_bufpool::PoolSlot<TextKey>,
            }
        });
        assert!(output.contains("HasPool < PoolAlias < Tag , VecKey < u8 , 1 > > > for Pools"));
        assert!(output.contains("r#type : context . slot"));
        assert!(output.contains("HasPool < TextKey > for Pools"));
    }

    #[kithara::test(native)]
    #[case::enumeration(parse_quote! { enum Pools { Bytes } }, "named `PoolSlot<K>` fields")]
    #[case::tuple(parse_quote! { struct Pools(PoolSlot<u8>); }, "named `PoolSlot<K>` fields")]
    #[case::unit(parse_quote! { struct Pools; }, "named `PoolSlot<K>` fields")]
    #[case::empty(parse_quote! { struct Pools {} }, "at least one")]
    #[case::generic(parse_quote! { struct Pools<K> { bytes: PoolSlot<K> } }, "concrete")]
    #[case::lifetime(parse_quote! { struct Pools<'a> { bytes: PoolSlot<&'a u8> } }, "concrete")]
    #[case::visible(parse_quote! { struct Pools { pub bytes: PoolSlot<u8> } }, "`bytes` must be private")]
    #[case::scalar(parse_quote! { struct Pools { bytes: u8 } }, "`PoolSlot<K>`")]
    #[case::bare_slot(parse_quote! { struct Pools { bytes: PoolSlot } }, "`PoolSlot<K>`")]
    #[case::two_keys(parse_quote! { struct Pools { bytes: PoolSlot<u8, f32> } }, "`PoolSlot<K>`")]
    #[case::other_wrapper(parse_quote! { struct Pools { bytes: Slot<u8> } }, "`PoolSlot<K>`")]
    #[case::builder(parse_quote! { struct Pools { builder: PoolSlot<u8> } }, "`builder` collides")]
    #[case::build(parse_quote! { struct Pools { build: PoolSlot<u8> } }, "`build` collides")]
    #[case::hidden(parse_quote! { struct Pools { __build_region: PoolSlot<u8> } }, "collides")]
    #[case::budget(parse_quote! { struct Pools { r#overall_budget: PoolSlot<u8> } }, "collides")]
    fn refuses_unsupported_schemas(#[case] input: DeriveInput, #[case] message: &str) {
        let refusal = refusal(&input);
        assert!(refusal.contains(message), "{refusal}");
    }
}

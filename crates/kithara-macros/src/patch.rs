//! Expansion of `#[derive(Patch)]`.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{
    Attribute, Data, DeriveInput, Error, Field, Fields, Ident, PathArguments, Result, Token, Type,
    Visibility, parenthesized, parse_macro_input, parse_quote, punctuated::Punctuated,
    spanned::Spanned,
};

pub(crate) fn expand(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match derive(&input) {
        Ok(tokens) => tokens.into(),
        Err(error) => error.to_compile_error().into(),
    }
}

/// How one source field reaches the generated patch.
enum Merge {
    /// The field's own type is a configuration with a patch of its own: the
    /// document names it under a key of the same name and the merge recurses.
    Nested,
    /// The source field is already an `Option`, so the patch carries it
    /// unwrapped. A document names the value bare, and an absent key is the
    /// only way to leave the caller's value standing.
    Optional,
    /// Everything else: the patch wraps the field's type in `Option`.
    Value,
}

/// One source field as it reaches the generated patch.
struct DocumentField<'a> {
    ident: &'a Ident,
    /// `doc` and `cfg` attributes the source field already carries.
    forwarded: Vec<&'a Attribute>,
    /// Attributes `#[patch(attribute(...))]` adds to the patch field alone.
    added: Vec<TokenStream2>,
    merge: Merge,
    ty: Type,
}

impl DocumentField<'_> {
    /// A patch key is exactly as reachable as the patch that holds it: the
    /// source field's own visibility describes who may write the
    /// configuration, not who may read what a document said.
    fn declaration(&self, visibility: &Visibility) -> TokenStream2 {
        let Self {
            ident,
            forwarded,
            added,
            ty,
            ..
        } = self;
        quote! {
            #(#forwarded)*
            #( #[#added] )*
            #visibility #ident: #ty,
        }
    }

    fn merge_statement(&self) -> TokenStream2 {
        let ident = self.ident;
        let cfgs = self
            .forwarded
            .iter()
            .filter(|attribute| attribute.path().is_ident("cfg"));
        let merge = match self.merge {
            Merge::Nested => quote! { self.#ident.apply(patch.#ident); },
            Merge::Optional => quote! {
                if patch.#ident.is_some() {
                    self.#ident = patch.#ident;
                }
            },
            Merge::Value => quote! {
                if let Some(value) = patch.#ident {
                    self.#ident = value;
                }
            },
        };
        quote! { #(#cfgs)* #merge }
    }
}

fn derive(input: &DeriveInput) -> Result<TokenStream2> {
    let document = named_fields(input)?
        .iter()
        .filter_map(|field| document_field(field).transpose())
        .collect::<Result<Vec<_>>>()?;

    let name = &input.ident;
    let patch = format_ident!("{name}Patch");
    let visibility = &input.vis;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();
    let subject = format!(" What a configuration document may say about [`{name}`].");

    let declarations = document.iter().map(|field| field.declaration(visibility));
    let merges = document.iter().map(DocumentField::merge_statement);

    Ok(quote! {
        #[doc = #subject]
        #[doc = ""]
        #[doc = " `Deserialize` only, never `Serialize`: by the time a patch is typed"]
        #[doc = " its references are resolved, so it holds secrets in the clear."]
        #[derive(
            ::core::clone::Clone,
            ::core::fmt::Debug,
            ::core::default::Default,
            ::serde::Deserialize,
        )]
        #[serde(default, deny_unknown_fields)]
        #[non_exhaustive]
        #visibility struct #patch {
            #(#declarations)*
        }

        #[automatically_derived]
        impl #impl_generics #name #ty_generics #where_clause {
            #[doc = " Merge what a configuration document said onto this configuration."]
            #[doc = ""]
            #[doc = " A key the document did not name leaves the value already in place."]
            #visibility fn apply(&mut self, patch: #patch) {
                #(#merges)*
            }
        }
    })
}

fn named_fields(input: &DeriveInput) -> Result<&Punctuated<Field, Token![,]>> {
    let Data::Struct(data) = &input.data else {
        return Err(Error::new(
            input.ident.span(),
            "a configuration document describes a struct, so `Patch` derives on one",
        ));
    };
    let Fields::Named(named) = &data.fields else {
        return Err(Error::new(
            input.ident.span(),
            "a configuration document names its keys, so `Patch` needs named fields",
        ));
    };
    Ok(&named.named)
}

fn document_field(field: &Field) -> Result<Option<DocumentField<'_>>> {
    let mut skip = false;
    let mut nested = false;
    let mut added: Vec<TokenStream2> = Vec::new();

    for attribute in field.attrs.iter().filter(|a| a.path().is_ident("patch")) {
        attribute.parse_nested_meta(|meta| {
            if meta.path.is_ident("skip") {
                skip = true;
            } else if meta.path.is_ident("nested") {
                nested = true;
            } else if meta.path.is_ident("attribute") {
                let content;
                parenthesized!(content in meta.input);
                added.push(content.parse()?);
            } else {
                return Err(meta.error("expected `skip`, `nested` or `attribute(...)`"));
            }
            Ok(())
        })?;
    }

    if skip {
        return Ok(None);
    }

    let Some(ident) = field.ident.as_ref() else {
        return Err(Error::new(
            field.span(),
            "a document key needs a field name",
        ));
    };

    let ty = &field.ty;
    let (merge, ty) = if nested {
        (Merge::Nested, nested_patch_type(ty)?)
    } else if is_option(ty) {
        (Merge::Optional, ty.clone())
    } else {
        (Merge::Value, parse_quote!(::core::option::Option<#ty>))
    };

    Ok(Some(DocumentField {
        ident,
        forwarded: field
            .attrs
            .iter()
            .filter(|a| a.path().is_ident("doc") || a.path().is_ident("cfg"))
            .collect(),
        added,
        merge,
        ty,
    }))
}

/// `kithara_beat::BeatConfig` becomes `kithara_beat::BeatConfigPatch`.
fn nested_patch_type(ty: &Type) -> Result<Type> {
    let Type::Path(mut path) = ty.clone() else {
        return Err(Error::new(
            ty.span(),
            "`nested` needs a named configuration type",
        ));
    };
    let Some(last) = path.path.segments.last_mut() else {
        return Err(Error::new(ty.span(), "`nested` needs a named type"));
    };
    last.ident = format_ident!("{}Patch", last.ident);
    last.arguments = PathArguments::None;
    Ok(Type::Path(path))
}

fn is_option(ty: &Type) -> bool {
    let Type::Path(path) = ty else {
        return false;
    };
    path.path
        .segments
        .last()
        .is_some_and(|last| last.ident == "Option")
}

#[cfg(test)]
mod tests {
    use syn::{DeriveInput, parse_quote};

    use super::derive;

    fn expansion(input: &DeriveInput) -> String {
        derive(input).expect("the derive expands").to_string()
    }

    #[test]
    fn a_generic_configuration_yields_a_patch_with_no_generics() {
        let input: DeriveInput = parse_quote! {
            pub struct HlsConfig<S> {
                #[patch(skip)]
                pub store: AssetStore<S>,
                pub download_batch_size: usize,
            }
        };

        let expanded = expansion(&input);

        assert!(expanded.contains("pub struct HlsConfigPatch { pub download_batch_size"));
        assert!(
            !expanded.contains("HlsConfigPatch <"),
            "the patch must not repeat the configuration's generics"
        );
        assert!(
            !expanded.contains("store"),
            "a skipped field reaches neither the patch nor the merge"
        );
    }

    #[test]
    fn an_already_optional_field_is_wrapped_once() {
        let input: DeriveInput = parse_quote! {
            struct Config {
                look_ahead_bytes: Option<u64>,
                batch: usize,
            }
        };

        let expanded = expansion(&input);

        assert!(expanded.contains("look_ahead_bytes : Option < u64 >"));
        assert!(expanded.contains("batch : :: core :: option :: Option < usize >"));
    }

    #[test]
    fn a_nested_field_recurses_into_the_owning_crates_patch() {
        let input: DeriveInput = parse_quote! {
            struct Config {
                #[patch(nested)]
                beat: kithara_beat::BeatConfig,
            }
        };

        let expanded = expansion(&input);

        assert!(expanded.contains("beat : kithara_beat :: BeatConfigPatch"));
        assert!(expanded.contains("self . beat . apply (patch . beat)"));
    }

    #[test]
    fn a_gated_field_gates_its_merge_too() {
        let input: DeriveInput = parse_quote! {
            struct Config {
                #[cfg(feature = "beat-nn")]
                threshold: f32,
            }
        };

        let expanded = expansion(&input);

        assert_eq!(
            expanded.matches("cfg (feature = \"beat-nn\")").count(),
            2,
            "the patch field and the merge statement both carry the gate"
        );
    }

    #[test]
    fn an_unknown_option_names_what_the_derive_accepts() {
        let input: DeriveInput = parse_quote! {
            struct Config {
                #[patch(rename = "other")]
                batch: usize,
            }
        };

        let error = derive(&input).expect_err("the derive refuses the option");

        assert!(
            error
                .to_string()
                .contains("expected `skip`, `nested` or `attribute(...)`")
        );
    }
}

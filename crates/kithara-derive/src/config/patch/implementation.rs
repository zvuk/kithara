//! Expansion of `#[derive(Patch)]`.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{
    Attribute, Data, DeriveInput, Error, Field, Fields, Ident, Path, PathArguments, Result, Token,
    Type, Visibility, parenthesized, parse_macro_input, parse_quote, punctuated::Punctuated,
    spanned::Spanned,
};

use super::attribute;

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
    /// A nested configuration that judges itself refuses through its own
    /// patch error, which this one carries under the key that reached it.
    Nested { fallible: bool },
    /// The field holds something a document cannot spell -- a live handle, a
    /// variant carrying one -- and a separate wire type is what a document
    /// says instead. The key carries the wire type and the merge converts.
    Wire { from: Path },
    /// Everything else: the patch wraps the field's type in `Option`.
    Value,
}

/// One source field as it reaches the generated patch.
struct DocumentField<'a> {
    ident: &'a Ident,
    deserialize: Option<Path>,
    merge: Merge,
    /// The field's own type, before the patch wraps or renames it.
    source: Type,
    ty: Type,
    /// Attributes `#[patch(attribute(...))]` adds to the patch field alone.
    added: Vec<TokenStream2>,
    /// `doc` and `cfg` attributes the source field already carries.
    forwarded: Vec<&'a Attribute>,
}

/// What the struct as a whole said about refusing a merged configuration.
struct Refusal {
    /// `fn(Self) -> Result<Self, Error>`, the one gate every route in holds,
    /// when the configuration judges itself as a whole.
    validate: Option<Check>,
}

/// The check a configuration puts every merged candidate through.
struct Check {
    with: Path,
    /// What that gate refuses with.
    error: Type,
}

/// One source field, sorted by whether a document may name it.
enum Classified<'a> {
    Key(Box<DocumentField<'a>>),
    Skipped,
}

fn cfgs(attributes: &[&Attribute]) -> Vec<TokenStream2> {
    attributes
        .iter()
        .filter(|attribute| attribute.path().is_ident("cfg"))
        .map(|attribute| quote! { #attribute })
        .collect()
}

/// `tempo` reaches the generated error as `Tempo`.
fn variant_ident(ident: &Ident) -> Ident {
    let mut name = String::new();
    let mut capitalise = true;
    for character in ident.to_string().chars() {
        if character == '_' {
            capitalise = true;
        } else if capitalise {
            name.extend(character.to_uppercase());
            capitalise = false;
        } else {
            name.push(character);
        }
    }
    format_ident!("{name}", span = ident.span())
}

impl DocumentField<'_> {
    fn cfgs(&self) -> Vec<TokenStream2> {
        cfgs(&self.forwarded)
    }

    /// A patch key is exactly as reachable as the patch that holds it: the
    /// source field's own visibility describes who may write the
    /// configuration, not who may read what a document said.
    fn declaration(&self, visibility: &Visibility, patch: &Ident) -> TokenStream2 {
        let Self {
            ident,
            forwarded,
            added,
            ty,
            ..
        } = self;
        let deserialize = if matches!(self.merge, Merge::Nested { .. }) {
            quote! {}
        } else {
            let method = format_ident!("__deserialize_{}", ident);
            let path = format!("{patch}::{method}");
            quote! { #[serde(deserialize_with = #path)] }
        };
        quote! {
            #(#forwarded)*
            #( #[#added] )*
            #deserialize
            #visibility #ident: #ty,
        }
    }

    fn deserializer(&self) -> TokenStream2 {
        if matches!(self.merge, Merge::Nested { .. }) {
            return quote! {};
        }
        let method = format_ident!("__deserialize_{}", self.ident);
        let ty = &self.ty;
        let gates = self.cfgs();
        let default = parse_quote!(::serde::Deserialize::deserialize);
        let deserialize = self.deserialize.as_ref().unwrap_or(&default);
        let read = if is_option(&self.source) {
            quote! { #deserialize(deserializer).map(::core::option::Option::Some) }
        } else {
            quote! {
                let value: #ty = #deserialize(deserializer)?;
                value.map(::core::option::Option::Some).ok_or_else(|| {
                    ::serde::de::Error::custom("null is not valid for a required configuration field")
                })
            }
        };
        quote! {
            #(#gates)*
            fn #method<'de, D>(deserializer: D) -> ::core::result::Result<#ty, D::Error>
            where
                D: ::serde::Deserializer<'de>,
            {
                #read
            }
        }
    }

    fn is_fallible(&self) -> bool {
        matches!(self.merge, Merge::Nested { fallible: true })
    }

    /// This key's merge, written onto `target` -- the caller's own value when
    /// nothing can refuse it, and a staged copy when the whole is judged
    /// before it is committed.
    fn merge_statement(&self, target: &TokenStream2, error: &Ident) -> TokenStream2 {
        let ident = self.ident;
        let cfgs = self.cfgs();
        let merge = match self.merge {
            Merge::Nested { fallible: false } => {
                quote! { #target.#ident.apply(patch.#ident); }
            }
            Merge::Nested { fallible: true } => {
                let variant = variant_ident(ident);
                quote! { #target.#ident.apply(patch.#ident).map_err(#error::#variant)?; }
            }
            Merge::Wire { ref from } => quote! {
                if let Some(value) = patch.#ident {
                    #target.#ident = #from(value);
                }
            },
            Merge::Value => quote! {
                if let Some(value) = patch.#ident {
                    #target.#ident = value;
                }
            },
        };
        quote! { #(#cfgs)* #merge }
    }
}

fn derive(input: &DeriveInput) -> Result<TokenStream2> {
    let refusal = refusal(input)?;
    let mut document: Vec<DocumentField<'_>> = Vec::new();
    for field in named_fields(input)? {
        if let Classified::Key(key) = classify(field)? {
            document.push(*key);
        }
    }

    if refusal.is_none()
        && let Some(field) = document.iter().find(|field| field.is_fallible())
    {
        return Err(Error::new(
            field.ident.span(),
            "a merge that carries a nested refusal is one the struct declares: add `#[patch(fallible)]`",
        ));
    }

    let name = &input.ident;
    let patch = format_ident!("{name}Patch");
    let error = format_ident!("{name}PatchError");
    let visibility = &input.vis;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();
    let subject = format!(" What a configuration document may say about [`{name}`].");

    let declarations = document
        .iter()
        .map(|field| field.declaration(visibility, &patch));
    let deserializers = document.iter().map(DocumentField::deserializer);
    let refusals = refusals(&document, refusal.as_ref(), &error, visibility)?;
    let apply = apply(&document, refusal.as_ref(), &patch, &error, visibility);

    Ok(quote! {
        #[doc = #subject]
        #[doc = ""]
        #[doc = " `Deserialize` only, never `Serialize`: by the time a patch is typed"]
        #[doc = " its references are resolved, so it holds secrets in the clear."]
        #[doc = " Missing keys preserve values. Null clears optional values and is rejected for required values."]
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
        impl #patch {
            #(#deserializers)*
        }

        #refusals

        #[automatically_derived]
        impl #impl_generics #name #ty_generics #where_clause {
            #apply
        }
    })
}

/// The error the generated merge refuses with, and the impls that name the key
/// it refused under. Nothing is emitted for a configuration that cannot refuse.
///
/// A declared refusal with nothing left to refuse — every fallible key gated
/// out by the current features — still emits the type, uninhabited. The
/// signature is the struct's declaration, not the feature set's.
fn refusals(
    document: &[DocumentField<'_>],
    refusal: Option<&Refusal>,
    error: &Ident,
    visibility: &Visibility,
) -> Result<TokenStream2> {
    let Some(refusal) = refusal else {
        return Ok(TokenStream2::new());
    };
    let fallible = document.iter().filter(|field| field.is_fallible());

    let subject =
        format!(" Why a configuration document is not one [`{error}`]'s subject accepts.");
    let mut variants: Vec<TokenStream2> = Vec::new();
    let mut displays: Vec<TokenStream2> = Vec::new();
    let mut sources: Vec<TokenStream2> = Vec::new();

    if let Some(Check { error: refused, .. }) = &refusal.validate {
        variants.push(quote! {
            #[doc = " The merged configuration is not one this type accepts."]
            Invalid(#refused),
        });
        displays.push(quote! {
            Self::Invalid(ref source) => ::core::fmt::Display::fmt(source, formatter),
        });
        sources.push(quote! { Self::Invalid(ref source) => ::core::option::Option::Some(source), });
    }

    for field in fallible {
        let variant = variant_ident(field.ident);
        let key = field.ident.to_string();
        let refused = patch_error_type(&field.source)?;
        let cfgs = field.cfgs();
        let doc =
            format!(" The document's `{key}` section was refused by the configuration it names.");
        variants.push(quote! { #[doc = #doc] #(#cfgs)* #variant(#refused), });
        displays.push(quote! {
            #(#cfgs)*
            Self::#variant(ref source) => ::core::write!(formatter, "{}: {source}", #key),
        });
        sources.push(quote! {
            #(#cfgs)*
            Self::#variant(ref source) => ::core::option::Option::Some(source),
        });
    }

    let formatter = if displays.is_empty() {
        format_ident!("_formatter")
    } else {
        format_ident!("formatter")
    };

    Ok(quote! {
        #[doc = #subject]
        #[doc = ""]
        #[doc = " Displays as the document key that was refused, then what refused it,"]
        #[doc = " so a nested refusal reads as the path a document would have to fix."]
        #[derive(::core::fmt::Debug)]
        #[non_exhaustive]
        #visibility enum #error {
            #(#variants)*
        }

        #[automatically_derived]
        impl ::core::fmt::Display for #error {
            fn fmt(&self, #formatter: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                match *self {
                    #(#displays)*
                }
            }
        }

        #[automatically_derived]
        impl ::core::error::Error for #error {
            fn source(&self) -> ::core::option::Option<&(dyn ::core::error::Error + 'static)> {
                match *self {
                    #(#sources)*
                }
            }
        }
    })
}

/// The merge itself. A configuration that can refuse builds a candidate beside
/// the caller's value and only commits a judged one, so a refused document
/// leaves the caller holding exactly what it had.
fn apply(
    document: &[DocumentField<'_>],
    refusal: Option<&Refusal>,
    patch: &Ident,
    error: &Ident,
    visibility: &Visibility,
) -> TokenStream2 {
    let merge = " Merge what a configuration document said onto this configuration.";
    let unnamed = " A key the document did not name leaves the value already in place.";

    let Some(refusal) = refusal else {
        let target = quote! { self };
        let merges = document
            .iter()
            .map(|field| field.merge_statement(&target, error));
        return quote! {
            #[doc = #merge]
            #[doc = ""]
            #[doc = #unnamed]
            #visibility fn apply(&mut self, patch: #patch) {
                #(#merges)*
            }
        };
    };

    let refuses = format!(
        " [`{error}`] when the merged configuration is one this type refuses,\n \
         naming the document key that carried the refused value."
    );
    let body = if let Some(Check { with, .. }) = &refusal.validate {
        let target = quote! { staged };
        let merges = document
            .iter()
            .map(|field| field.merge_statement(&target, error));
        quote! {
            let mut staged = ::core::clone::Clone::clone(&*self);
            #(#merges)*
            *self = #with(staged).map_err(#error::Invalid)?;
        }
    } else {
        let fallible: Vec<&DocumentField<'_>> = document
            .iter()
            .filter(|field| field.is_fallible())
            .collect();
        let judged = fallible.iter().map(|field| {
            let ident = field.ident;
            let local = format_ident!("{ident}_merged");
            let variant = variant_ident(ident);
            let cfgs = field.cfgs();
            quote! {
                #(#cfgs)*
                let #local = {
                    let mut value = ::core::clone::Clone::clone(&self.#ident);
                    value.apply(patch.#ident).map_err(#error::#variant)?;
                    value
                };
            }
        });
        let commits = fallible.iter().map(|field| {
            let ident = field.ident;
            let local = format_ident!("{ident}_merged");
            let cfgs = field.cfgs();
            quote! { #(#cfgs)* { self.#ident = #local; } }
        });
        let target = quote! { self };
        let merges = document
            .iter()
            .filter(|field| !field.is_fallible())
            .map(|field| field.merge_statement(&target, error));
        quote! {
            #(#judged)*
            #(#commits)*
            #(#merges)*
        }
    };

    quote! {
        #[doc = #merge]
        #[doc = ""]
        #[doc = #unnamed]
        #[doc = ""]
        #[doc = " # Errors"]
        #[doc = #refuses]
        #visibility fn apply(
            &mut self,
            patch: #patch,
        ) -> ::core::result::Result<(), #error> {
            #body
            ::core::result::Result::Ok(())
        }
    }
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

/// What `#[patch(...)]` on the struct itself said.
///
/// The declaration is the struct's, not a consequence of the fields the
/// current features leave standing: `cfg` is resolved before this derive runs,
/// so a gated fallible field is simply absent here, and inferring the
/// signature from what is left would move it from build to build.
fn refusal(input: &DeriveInput) -> Result<Option<Refusal>> {
    refusal_from_attributes(&input.attrs, input.ident.span())
}

pub(crate) fn is_fallible(attributes: &[Attribute], span: proc_macro2::Span) -> Result<bool> {
    refusal_from_attributes(attributes, span).map(|refusal| refusal.is_some())
}

fn refusal_from_attributes(
    attributes: &[Attribute],
    span: proc_macro2::Span,
) -> Result<Option<Refusal>> {
    let mut fallible = false;
    let mut with: Option<Path> = None;
    let mut error: Option<Type> = None;

    for attribute in attributes.iter().filter(|a| a.path().is_ident("patch")) {
        attribute.parse_nested_meta(|meta| {
            if meta.path.is_ident("fallible") {
                fallible = true;
            } else if meta.path.is_ident("validate") {
                with = Some(meta.value()?.parse()?);
            } else if meta.path.is_ident("error") {
                error = Some(meta.value()?.parse()?);
            } else {
                return Err(
                    meta.error("expected `fallible`, `validate = <path>` or `error = <type>`")
                );
            }
            Ok(())
        })?;
    }

    match (with, error) {
        (Some(with), Some(error)) => Ok(Some(Refusal {
            validate: Some(Check { with, error }),
        })),
        (None, None) if fallible => Ok(Some(Refusal { validate: None })),
        (None, None) => Ok(None),
        (Some(_), None) => Err(Error::new(
            span,
            "`validate` needs `error = <type>`: the generated patch error carries what the check refused with",
        )),
        (None, Some(_)) => Err(Error::new(
            span,
            "`error` needs `validate = <path>`: without a check nothing in the merge can refuse",
        )),
    }
}

fn classify(field: &Field) -> Result<Classified<'_>> {
    let mut skip = false;
    let mut deserialize = None;
    let mut nested = false;
    let mut fallible = false;
    let mut wire: Option<Type> = None;
    let mut from: Option<Path> = None;
    let mut added: Vec<TokenStream2> = Vec::new();

    for attribute in field.attrs.iter().filter(|a| a.path().is_ident("patch")) {
        attribute.parse_nested_meta(|meta| {
            if meta.path.is_ident("skip") {
                skip = true;
            } else if meta.path.is_ident("nested") {
                nested = true;
            } else if meta.path.is_ident("fallible") {
                fallible = true;
            } else if meta.path.is_ident("wire") {
                wire = Some(meta.value()?.parse()?);
            } else if meta.path.is_ident("from") {
                from = Some(meta.value()?.parse()?);
            } else if meta.path.is_ident("attribute") {
                let content;
                parenthesized!(content in meta.input);
                attribute::collect(&content.parse()?, &mut added, &mut deserialize)?;
            } else if meta.path.is_ident("humantime") {
                attribute::set(
                    &mut deserialize,
                    parse_quote!(::humantime_serde::deserialize),
                )?;
            } else {
                return Err(meta.error(
                    "expected `skip`, `nested`, `fallible`, `wire = <type>`, `from = <path>`, \
                     `humantime` or `attribute(...)`",
                ));
            }
            Ok(())
        })?;
    }

    let Some(ident) = field.ident.as_ref() else {
        return Err(Error::new(
            field.span(),
            "a document key needs a field name",
        ));
    };

    if skip {
        return Ok(Classified::Skipped);
    }

    if fallible && !nested {
        return Err(Error::new(
            field.span(),
            "`fallible` describes a nested configuration that judges itself, so it needs `nested`",
        ));
    }

    if deserialize.is_some() && nested {
        return Err(Error::new(
            field.span(),
            "a custom field deserializer cannot replace a nested patch",
        ));
    }
    let wired = wire_of(field, wire, from)?;

    if wired.is_some() && nested {
        return Err(Error::new(
            field.span(),
            "`wire` replaces the field's type on the document side, so it has no nested patch to \
             recurse into: drop one of `wire` and `nested`",
        ));
    }

    let source = field.ty.clone();
    let (merge, ty) = if let Some((wire, from)) = wired {
        (
            Merge::Wire { from },
            parse_quote!(::core::option::Option<#wire>),
        )
    } else if nested {
        (
            Merge::Nested { fallible },
            renamed(
                &source,
                "Patch",
                "`nested` needs a named configuration type",
            )?,
        )
    } else {
        (Merge::Value, parse_quote!(::core::option::Option<#source>))
    };

    Ok(Classified::Key(Box::new(DocumentField {
        ident,
        added,
        deserialize,
        merge,
        source,
        ty,
        forwarded: field
            .attrs
            .iter()
            .filter(|a| a.path().is_ident("doc") || a.path().is_ident("cfg"))
            .collect(),
    })))
}

/// What a field said about travelling as a type of its own. Both halves are
/// required: the wire type alone leaves the merge with nothing to convert
/// with, and the conversion alone leaves the key without a type to parse.
fn wire_of(field: &Field, wire: Option<Type>, from: Option<Path>) -> Result<Option<(Type, Path)>> {
    match (wire, from) {
        (Some(wire), Some(from)) => Ok(Some((wire, from))),
        (None, None) => Ok(None),
        (Some(_), None) => Err(Error::new(
            field.span(),
            "`wire` needs `from = <path>`: the merge converts what a document said into the \
             field's own type",
        )),
        (None, Some(_)) => Err(Error::new(
            field.span(),
            "`from` needs `wire = <type>`: without one the key has no type a document can name",
        )),
    }
}

/// `kithara_beat::Tempo` becomes `kithara_beat::TempoPatchError`.
fn patch_error_type(ty: &Type) -> Result<Type> {
    renamed(
        ty,
        "PatchError",
        "`fallible` needs a named configuration type",
    )
}

/// `kithara_beat::BeatConfig` becomes `kithara_beat::BeatConfig{suffix}`.
fn renamed(ty: &Type, suffix: &str, refusal: &str) -> Result<Type> {
    let Type::Path(mut path) = ty.clone() else {
        return Err(Error::new(ty.span(), refusal));
    };
    let Some(last) = path.path.segments.last_mut() else {
        return Err(Error::new(ty.span(), refusal));
    };
    last.ident = format_ident!("{}{suffix}", last.ident);
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
    use kithara_test_utils::kithara;
    use syn::{DeriveInput, parse_quote};

    use super::derive;

    fn expansion(input: &DeriveInput) -> String {
        derive(input).expect("the derive expands").to_string()
    }

    #[kithara::test(native, flash(false))]
    fn a_generic_configuration_yields_a_patch_with_no_generics() {
        let input: DeriveInput = parse_quote! {
            pub struct HlsConfig<S> {
                #[patch(skip)]
                pub store: AssetStore<S>,
                pub download_batch_size: usize,
            }
        };

        let expanded = expansion(&input);

        assert!(expanded.contains("pub struct HlsConfigPatch {"));
        assert!(expanded.contains("pub download_batch_size :"));
        assert!(
            !expanded.contains("HlsConfigPatch <"),
            "the patch must not repeat the configuration's generics"
        );
        assert!(
            !expanded.contains("store"),
            "a skipped field reaches neither the patch nor the merge"
        );
    }

    #[kithara::test(native, flash(false))]
    fn optional_fields_keep_presence_separate_from_their_value() {
        let input: DeriveInput = parse_quote! {
            struct Config {
                look_ahead_bytes: Option<u64>,
                batch: usize,
            }
        };

        let expanded = expansion(&input);

        assert!(
            expanded.contains("look_ahead_bytes : :: core :: option :: Option < Option < u64 > >")
        );
        assert!(expanded.contains("batch : :: core :: option :: Option < usize >"));
    }

    #[kithara::test(native, flash(false))]
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

    #[kithara::test(native, flash(false))]
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
            3,
            "the patch field, its deserializer and the merge carry the gate"
        );
    }

    #[kithara::test(native, flash(false))]
    fn an_unknown_option_names_what_the_derive_accepts() {
        let input: DeriveInput = parse_quote! {
            struct Config {
                #[patch(rename = "other")]
                batch: usize,
            }
        };

        let error = derive(&input).expect_err("the derive refuses the option");

        assert!(
            error.to_string().contains(
                "expected `skip`, `nested`, `fallible`, `wire = <type>`, `from = <path>`, \
                 `humantime` or `attribute(...)`"
            ),
            "{error}"
        );
    }

    #[kithara::test(native, flash(false))]
    fn conflicting_field_deserializers_are_rejected() {
        let input: DeriveInput = parse_quote! {
            struct Config {
                #[patch(humantime, attribute(serde(with = "custom_time")))]
                timeout: Duration,
            }
        };
        let error = derive(&input).expect_err("two field codecs cannot both own deserialization");
        assert!(
            error
                .to_string()
                .contains("select one patch field deserializer")
        );
    }

    #[kithara::test(native, flash(false))]
    fn humantime_is_the_supported_duration_shorthand() {
        let input: DeriveInput = parse_quote! {
            struct Config {
                #[patch(humantime)]
                timeout: Duration,
            }
        };

        let expanded = expansion(&input);
        assert!(expanded.contains(":: humantime_serde :: deserialize (deserializer)"));
    }

    #[kithara::test(native, flash(false))]
    fn a_configuration_that_judges_itself_commits_only_a_judged_whole() {
        let input: DeriveInput = parse_quote! {
            #[patch(validate = Self::validated, error = TempoError)]
            pub struct Tempo {
                low: f32,
                high: f32,
            }
        };

        let expanded = expansion(&input);

        assert!(
            expanded.contains("-> :: core :: result :: Result < () , TempoPatchError >"),
            "the merge refuses through the generated error"
        );
        assert!(
            expanded.contains("let mut staged = :: core :: clone :: Clone :: clone (& * self)"),
            "the merge is staged beside the caller's value"
        );
        assert!(
            expanded.contains("staged . low = value"),
            "the staged copy is what the document's keys are written onto"
        );
        assert!(
            expanded.contains("* self = Self :: validated (staged)"),
            "only a judged whole is committed"
        );
        assert!(
            expanded.contains("Invalid (TempoError)"),
            "the generated error carries what the check refused with"
        );
    }

    #[kithara::test(native, flash(false))]
    fn a_skipped_field_is_carried_into_the_judged_whole_untouched() {
        let input: DeriveInput = parse_quote! {
            #[patch(validate = Self::validated, error = Refusal)]
            pub struct Config {
                #[patch(skip)]
                backend: Backend,
                prior: f32,
            }
        };

        let expanded = expansion(&input);

        assert!(
            !expanded.contains("backend"),
            "staging the whole carries the fields no document may name, so none is named here"
        );
        assert!(
            expanded.contains("let mut staged = :: core :: clone :: Clone :: clone (& * self)"),
            "the field the check needs reaches it through the staged whole"
        );
    }

    #[kithara::test(native, flash(false))]
    fn a_fallible_nested_field_carries_its_refusal_under_its_own_key() {
        let input: DeriveInput = parse_quote! {
            #[patch(fallible)]
            pub struct BeatAnalysisConfig<B> {
                #[patch(skip)]
                resampler_backend: B,
                target_rate: u32,
                #[cfg(feature = "beat-dsp")]
                #[patch(nested, fallible)]
                pub tempo: kithara_beat::Tempo,
            }
        };

        let expanded = expansion(&input);

        assert!(
            expanded.contains("Tempo (kithara_beat :: TempoPatchError)"),
            "the parent's error carries the child's"
        );
        assert!(
            expanded.contains("\"tempo\""),
            "the refusal names the document key that carried it"
        );
        assert!(
            expanded.contains("map_err (BeatAnalysisConfigPatchError :: Tempo)"),
            "the child's refusal reaches the parent's error"
        );
        assert!(
            expanded.contains("{ self . tempo = tempo_merged ; }"),
            "a judged child is committed only after every child was judged"
        );
    }

    #[kithara::test(native, flash(false))]
    fn a_gated_fallible_field_gates_its_error_variant_too() {
        let input: DeriveInput = parse_quote! {
            #[patch(fallible)]
            pub struct Config {
                #[cfg(feature = "beat-dsp")]
                #[patch(nested, fallible)]
                pub tempo: Tempo,
            }
        };

        let expanded = expansion(&input);

        assert_eq!(
            expanded.matches("cfg (feature = \"beat-dsp\")").count(),
            6,
            "the patch field, the variant, its display and source arms, the merge \
             and its commit all carry the gate"
        );
    }

    #[kithara::test(native, flash(false))]
    fn a_configuration_that_cannot_refuse_keeps_an_infallible_merge() {
        let input: DeriveInput = parse_quote! {
            struct Config {
                batch: usize,
            }
        };

        let expanded = expansion(&input);

        assert!(
            expanded.contains("fn apply (& mut self , patch : ConfigPatch) { "),
            "nothing here can refuse, so the merge does not pretend it can"
        );
        assert!(
            !expanded.contains("ConfigPatchError"),
            "no error type is emitted for a configuration that cannot refuse"
        );
    }

    #[kithara::test(native, flash(false))]
    fn fallible_without_nested_is_refused() {
        let input: DeriveInput = parse_quote! {
            struct Config {
                #[patch(fallible)]
                batch: usize,
            }
        };

        let error = derive(&input).expect_err("the derive refuses the option");

        assert!(error.to_string().contains("needs `nested`"), "{error}");
    }

    #[kithara::test(native, flash(false))]
    fn a_nested_refusal_the_struct_did_not_declare_is_refused() {
        let input: DeriveInput = parse_quote! {
            struct Config {
                #[patch(nested, fallible)]
                tempo: Tempo,
            }
        };

        let error = derive(&input).expect_err("the derive refuses the undeclared refusal");

        assert!(
            error.to_string().contains("`#[patch(fallible)]`"),
            "{error}"
        );
    }

    #[kithara::test(native, flash(false))]
    fn a_declared_refusal_outlives_the_feature_that_gated_its_only_fallible_key() {
        let input: DeriveInput = parse_quote! {
            #[patch(fallible)]
            pub struct Config {
                pub target_rate: u32,
            }
        };

        let expanded = expansion(&input);

        assert!(
            expanded.contains("pub enum ConfigPatchError { }"),
            "the error the merge names survives having nothing left to refuse"
        );
        assert!(
            expanded.contains(":: core :: result :: Result < () , ConfigPatchError >"),
            "the merge keeps the signature the struct declared, not the one its features left"
        );
    }

    #[kithara::test(native, flash(false))]
    fn a_check_without_its_error_is_refused() {
        let input: DeriveInput = parse_quote! {
            #[patch(validate = Self::validated)]
            struct Config {
                batch: usize,
            }
        };

        let error = derive(&input).expect_err("the derive refuses the option");

        assert!(
            error.to_string().contains("needs `error = <type>`"),
            "{error}"
        );
    }

    #[kithara::test(native, flash(false))]
    fn a_wired_field_types_as_the_wire_and_lands_converted() {
        let input: DeriveInput = parse_quote! {
            struct WorkerConfig {
                #[patch(wire = ComputePool, from = PoolConfig::from)]
                pool: PoolConfig,
            }
        };

        let expanded = expansion(&input);

        assert!(
            expanded.contains("pool : :: core :: option :: Option < ComputePool >"),
            "the key carries the type a document can name, not the field's own"
        );
        assert!(
            expanded.contains("self . pool = PoolConfig :: from (value)"),
            "the merge converts before it writes"
        );
    }

    #[kithara::test(native, flash(false))]
    fn a_wire_without_its_conversion_is_refused() {
        let input: DeriveInput = parse_quote! {
            struct WorkerConfig {
                #[patch(wire = ComputePool)]
                pool: PoolConfig,
            }
        };

        let error = derive(&input).expect_err("the derive refuses the option");

        assert!(
            error.to_string().contains("needs `from = <path>`"),
            "{error}"
        );
    }

    #[kithara::test(native, flash(false))]
    fn a_wire_that_also_asks_to_recurse_is_refused() {
        let input: DeriveInput = parse_quote! {
            struct WorkerConfig {
                #[patch(nested, wire = ComputePool, from = PoolConfig::from)]
                pool: PoolConfig,
            }
        };

        let error = derive(&input).expect_err("the derive refuses the combination");

        assert!(
            error
                .to_string()
                .contains("no nested patch to recurse into"),
            "{error}"
        );
    }
}

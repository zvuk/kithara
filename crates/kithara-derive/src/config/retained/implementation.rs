use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::{Attribute, Data, DeriveInput, Fields, Ident, Result, Visibility, ext::IdentExt as _};

use super::{
    field::{self, Construction, Member},
    live,
};
use crate::config::{
    field::{Declaration, group},
    patch::{Check, declared_check, validation},
};

/// What `#[config(...)]` on the type itself declares.
#[derive(Default)]
struct Options {
    built_default: bool,
    construction: bool,
    fields: Declaration,
    /// `check(error = ...)`: the struct checks its fields.
    checked: bool,
    owner_access: bool,
    sdk: bool,
    debug: bool,
    values_vis: Option<Visibility>,
    /// bon's top-level options for the generated builder.
    builder: Option<TokenStream>,
    existing_builder: bool,
    no_builder: bool,
    validate_builder: bool,
}

impl Options {
    fn parse(item: &DeriveInput) -> Result<Self> {
        let mut options = Self::default();
        let mut seen: Vec<syn::Path> = Vec::new();
        for attr in item
            .attrs
            .iter()
            .filter(|attr| attr.path().is_ident("config"))
        {
            attr.parse_nested_meta(|meta| {
                if seen.contains(&meta.path) {
                    return Err(meta.error("duplicate config option"));
                }
                seen.push(meta.path.clone());
                let name = meta
                    .path
                    .get_ident()
                    .ok_or_else(|| meta.error("expected a config option name"))?
                    .to_string();
                match name.as_str() {
                    "default" => options.built_default = true,
                    "construction" => options.construction = true,
                    "check" => {
                        // `declared_check` reads the error type.
                        group(&meta)?;
                        options.checked = true;
                    }
                    "owner_access" => options.owner_access = true,
                    "validate_builder" => options.validate_builder = true,
                    "sdk" => options.sdk = true,
                    "debug" => options.debug = true,
                    "fields" => options.fields = Declaration::group(group(&meta)?)?,
                    "builder" => {
                        let group = group(&meta)?;
                        match group.to_string().as_str() {
                            "existing" => options.existing_builder = true,
                            "none" => options.no_builder = true,
                            _ => options.builder = Some(group),
                        }
                    }
                    "patch" => {
                        // `Patch` reads this group; the builder gate below reads its check.
                        group(&meta)?;
                    }
                    "values_vis" => {
                        let visibility: syn::LitStr = meta.value()?.parse()?;
                        options.values_vis = Some(syn::parse_str(&visibility.value())?);
                    }
                    _ => {
                        return Err(meta.error(
                            "expected construction, default, fields(...), check(...), validate_builder, sdk, debug, builder(...), \
                             owner_access, patch(...), or values_vis",
                        ));
                    }
                }
                Ok(())
            })?;
        }
        if options.construction
            && (options.built_default
                || options.checked
                || options.owner_access
                || options.sdk
                || options.values_vis.is_some())
        {
            return Err(syn::Error::new_spanned(
                &item.ident,
                "construction inputs cannot declare retained defaults, checks, owner access, SDK records, or values visibility",
            ));
        }
        if (options.existing_builder || options.no_builder) && options.validate_builder {
            return Err(syn::Error::new_spanned(
                &item.ident,
                "validate_builder requires a generated builder",
            ));
        }
        Ok(options)
    }
}

/// The `doc` and `cfg` attributes of a field, which every item generated from
/// it carries.
pub(super) fn docs(attributes: &[Attribute]) -> Vec<&Attribute> {
    attributes
        .iter()
        .filter(|attr| attr.path().is_ident("doc") || attr.path().is_ident("cfg"))
        .collect()
}

pub(crate) fn expand(input: TokenStream) -> Result<TokenStream> {
    let item: DeriveInput = syn::parse2(input)?;
    let options = Options::parse(&item)?;
    let Data::Struct(data) = &item.data else {
        return Err(syn::Error::new_spanned(
            &item.ident,
            "config requires a struct with named fields",
        ));
    };
    let Fields::Named(fields) = &data.fields else {
        return Err(syn::Error::new_spanned(
            &item.ident,
            "config requires named fields",
        ));
    };
    let members = fields
        .named
        .iter()
        .map(|field| field::expand(field, &item, !options.construction, &options.fields))
        .collect::<Result<Vec<_>>>()?;
    if options.owner_access && members.iter().all(|member| member.owner_accessor.is_none()) {
        return Err(syn::Error::new_spanned(
            &item.ident,
            "owner_access requires a get(ref) or get(copy) accessor",
        ));
    }
    if options.owner_access
        && let Some(member) = members
            .iter()
            .find(|member| member.owner_accessor.is_some() && member.name.unraw() == "config")
    {
        return Err(syn::Error::new_spanned(
            member.name,
            "owner_access cannot generate a getter named config, which ConfigOwner declares",
        ));
    }
    if !options.debug
        && let Some(member) = members.iter().find(|member| !member.debugged)
    {
        return Err(syn::Error::new_spanned(
            member.name,
            "debug(skip) requires `#[config(debug)]` on the type",
        ));
    }
    let check = validation(&item.attrs, item.ident.span())?;
    let error = declared_check(&item.attrs)?;
    validate_live(&item, &members, error.is_some(), check.is_some())?;
    let fallible = options.validate_builder;
    if fallible && check.is_none() {
        return Err(syn::Error::new_spanned(
            &item.ident,
            "validate_builder requires check(error = ...) or patch(validate = ..., error = ...)",
        ));
    }
    let builder = if options.existing_builder || options.no_builder {
        None
    } else {
        Some(builder(
            &item,
            &options,
            &members,
            if fallible { check } else { None },
        ))
    };
    let accessors = accessors(&item, &members);
    let owner_accessors = options
        .owner_access
        .then(|| owner_accessors(&item, &members))
        .flatten();
    let default = options
        .built_default
        .then(|| built_default(&item, fallible));
    let debug = options.debug.then(|| debug(&item, &members));
    let snapshot = (!options.construction).then(|| snapshot(&item, &options, &members));
    let live = live::expand(&item, &members, error.as_ref())?;
    Ok(quote! { #builder #accessors #owner_accessors #default #debug #snapshot #live })
}

/// Field checks need the struct's error type and live fields a struct whose
/// fields alone it checks: a whole-struct check would be skipped by a change
/// of one field.
fn validate_live(
    item: &DeriveInput,
    members: &[Member<'_>],
    declared: bool,
    whole: bool,
) -> Result<()> {
    if !declared && let Some(member) = members.iter().find(|member| member.check.is_some()) {
        return Err(syn::Error::new_spanned(
            member.name,
            "check = ... requires check(error = ...) on the struct",
        ));
    }
    if members.iter().all(|member| member.live.is_none()) {
        return Ok(());
    }
    if whole && !declared {
        return Err(syn::Error::new_spanned(
            &item.ident,
            "live fields require check(error = ...) instead of patch(validate = ...)",
        ));
    }
    if let Some(member) = members
        .iter()
        .find(|member| member.name.unraw() == "settings" || member.name.unraw() == "configure")
    {
        return Err(syn::Error::new_spanned(
            member.name,
            "a live configuration cannot name a field settings or configure",
        ));
    }
    if !item.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            &item.generics,
            "live fields require a configuration without generics",
        ));
    }
    distinct_changes(members)
}

/// Each live field names its own change variant, and nested live fields have
/// distinct types, because the parent's change converts from the nested one.
fn distinct_changes(members: &[Member<'_>]) -> Result<()> {
    let live: Vec<(&Member<'_>, &Ident)> = members
        .iter()
        .filter_map(|member| member.live.as_ref().map(|live| (member, &live.variant)))
        .collect();
    for (index, (member, variant)) in live.iter().enumerate() {
        let earlier = &live[..index];
        if let Some((first, _)) = earlier.iter().find(|(_, other)| other == variant) {
            let message = format!(
                "live fields `{}` and `{}` name the same change variant `{variant}`",
                first.name, member.name,
            );
            return Err(syn::Error::new_spanned(member.name, message));
        }
        if member.nested
            && let Some((first, _)) = earlier
                .iter()
                .find(|(other, _)| other.nested && other.ty == member.ty)
        {
            let message = format!(
                "nested live fields `{}` and `{}` share a type, so its change cannot name the field",
                first.name, member.name,
            );
            return Err(syn::Error::new_spanned(member.name, message));
        }
    }
    Ok(())
}

/// A bon function builder over `new`, which bon keeps private and hidden:
/// the type's constructor is `X::builder()`. Skipped fields are bound in
/// declaration order before any argument moves into `Self`, so their
/// expressions read the arguments and the skipped fields above them.
fn builder(
    item: &DeriveInput,
    options: &Options,
    members: &[Member<'_>],
    check: Option<Check>,
) -> TokenStream {
    let name = &item.ident;
    let visibility = &item.vis;
    let (impl_generics, ty_generics, where_clause) = item.generics.split_for_impl();
    let top = options
        .builder
        .as_ref()
        .map_or_else(|| quote!(#[builder]), |group| quote!(#[builder(#group)]));
    let arguments = members
        .iter()
        .filter_map(|member| match &member.construction {
            Construction::Argument(argument) => Some(argument),
            Construction::Initialised(_) => None,
        });
    let initialisers = members
        .iter()
        .filter_map(|member| match &member.construction {
            Construction::Argument(_) => None,
            Construction::Initialised(value) => {
                let (field, ty) = (member.name, member.ty);
                Some(quote!(let #field: #ty = #value;))
            }
        });
    let fields: Vec<&Ident> = members.iter().map(|member| member.name).collect();
    let (result, body) = if let Some(Check { with, error }) = check {
        (
            quote!(::core::result::Result<Self, #error>),
            quote!(#with(Self { #(#fields),* })),
        )
    } else {
        (quote!(Self), quote!(Self { #(#fields),* }))
    };
    quote! {
        #[::kithara_config::__private::bon::bon(crate = ::kithara_config::__private::bon)]
        #[automatically_derived]
        impl #impl_generics #name #ty_generics #where_clause {
            #top
            #visibility fn new(#(#arguments),*) -> #result {
                #(#initialisers)*
                #body
            }
        }
    }
}

fn accessors(item: &DeriveInput, members: &[Member<'_>]) -> Option<TokenStream> {
    let accessors: Vec<&TokenStream> = members
        .iter()
        .filter_map(|member| member.accessor.as_ref())
        .collect();
    if accessors.is_empty() {
        return None;
    }
    let name = &item.ident;
    let (impl_generics, ty_generics, where_clause) = item.generics.split_for_impl();
    Some(quote! {
        #[automatically_derived]
        impl #impl_generics #name #ty_generics #where_clause {
            #(#accessors)*
        }
    })
}

fn owner_accessors(item: &DeriveInput, members: &[Member<'_>]) -> Option<TokenStream> {
    let methods: Vec<&TokenStream> = members
        .iter()
        .filter_map(|member| member.owner_accessor.as_ref())
        .collect();
    if methods.is_empty() {
        return None;
    }
    let name = &item.ident;
    let visibility = &item.vis;
    let trait_name = format_ident!("{name}OwnerAccess");
    let (trait_generics, ty_generics, trait_where) = item.generics.split_for_impl();
    let mut blanket = item.generics.clone();
    blanket
        .params
        .insert(0, syn::parse_quote!(__KitharaConfigOwner));
    blanket
        .make_where_clause()
        .predicates
        .push(syn::parse_quote!(
            __KitharaConfigOwner: ::kithara_config::ConfigOwner<Config = #name #ty_generics>
        ));
    let (impl_generics, _, impl_where) = blanket.split_for_impl();
    Some(quote! {
        #visibility trait #trait_name #trait_generics:
            ::kithara_config::ConfigOwner<Config = #name #ty_generics> #trait_where
        {
            #(#methods)*
        }

        #[automatically_derived]
        impl #impl_generics #trait_name #ty_generics for __KitharaConfigOwner #impl_where {}
    })
}

fn built_default(item: &DeriveInput, fallible: bool) -> TokenStream {
    let name = &item.ident;
    let (impl_generics, ty_generics, where_clause) = item.generics.split_for_impl();
    // Declared defaults are an infallible contract; a rejected set is a code defect.
    let build = if fallible {
        quote! {
            Self::builder().build().unwrap_or_else(|_| {
                ::core::panic!(concat!("invalid declared defaults for ", stringify!(#name)))
            })
        }
    } else {
        quote!(Self::builder().build())
    };
    quote! {
        #[automatically_derived]
        impl #impl_generics ::core::default::Default for #name #ty_generics #where_clause {
            fn default() -> Self {
                #build
            }
        }
    }
}

/// `Debug` over the fields not marked `debug(skip)`; a debugged field whose
/// type names a type parameter bounds that type, not the parameter.
fn debug(item: &DeriveInput, members: &[Member<'_>]) -> TokenStream {
    let name = &item.ident;
    let (impl_generics, ty_generics, _) = item.generics.split_for_impl();
    let mut generics = item.generics.clone();
    let predicates = &mut generics.make_where_clause().predicates;
    let mut fields: Vec<TokenStream> = Vec::new();
    for member in members.iter().filter(|member| member.debugged) {
        let field = member.name;
        let ty = member.ty;
        if field::names_a_parameter(ty, &item.generics) {
            predicates.push(syn::parse_quote!(#ty: ::core::fmt::Debug));
        }
        fields.push(quote!(.field(::core::stringify!(#field), &self.#field)));
    }
    let finish = if fields.len() == members.len() {
        quote!(finish)
    } else {
        quote!(finish_non_exhaustive)
    };
    let where_clause = &generics.where_clause;
    quote! {
        #[automatically_derived]
        impl #impl_generics ::core::fmt::Debug for #name #ty_generics #where_clause {
            fn fmt(&self, formatter: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                formatter
                    .debug_struct(::core::stringify!(#name))
                    #(#fields)*
                    .#finish()
            }
        }
    }
}

fn snapshot(item: &DeriveInput, options: &Options, members: &[Member<'_>]) -> TokenStream {
    let name = &item.ident;
    let (value_fields, reads): (Vec<&TokenStream>, Vec<&TokenStream>) = members
        .iter()
        .filter_map(|member| member.retained.as_ref())
        .map(|retained| (&retained.declaration, &retained.read))
        .unzip();
    let values = format_ident!("{name}Values");
    let visibility = options.values_vis.as_ref().unwrap_or(&item.vis);
    let (impl_generics, ty_generics, where_clause) = item.generics.split_for_impl();
    quote! {
        #[doc = concat!("Owned readable values of `", stringify!(#name), "`. Resource inputs are excluded.")]
        #visibility struct #values {
            #(#value_fields,)*
        }
        #[automatically_derived]
        impl #impl_generics ::kithara_config::Config for #name #ty_generics #where_clause {
            type Values = #values;
            fn values(&self) -> Self::Values {
                #values { #(#reads,)* }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;
    use proc_macro2::TokenStream;
    use quote::quote;

    use super::expand;

    fn expansion(input: TokenStream) -> String {
        expand(input)
            .expect("a valid configuration expands")
            .to_string()
    }

    fn refusal(input: TokenStream) -> String {
        expand(input)
            .expect_err("an invalid configuration is refused")
            .to_string()
    }

    #[kithara::test(native, flash(false))]
    fn duplicate_options_are_rejected_instead_of_overriding_configuration() {
        for options in [
            quote!(default, default),
            quote!(debug, debug),
            quote!(fields(value), fields(nested)),
            quote!(fields(get(ref)), fields(get(copy))),
            quote!(builder(on(String, into)), builder(on(u32, into))),
            quote!(values_vis = "pub", values_vis = "pub(crate)"),
        ] {
            assert_eq!(
                refusal(quote! {
                    #[config(#options)]
                    struct Settings {
                        #[config(value)]
                        threshold: u32,
                    }
                }),
                "duplicate config option"
            );
        }
    }

    #[kithara::test(native, flash(false))]
    fn the_builder_is_a_bon_function_over_every_field_but_the_skipped_ones() {
        let expanded = expansion(quote! {
            #[config(builder(state_mod(vis = "pub")))]
            pub struct Settings {
                /// How far the ratio may go.
                #[config(value, builder(default = Consts::MAX_BAR_RATIO))]
                ratio: f64,
                #[config(skip = "set while running", builder(skip = Phase::Idle))]
                phase: Phase,
                #[config(skip = "counted while running", builder(skip))]
                count: usize,
            }
        });

        assert!(expanded.contains("# [builder (state_mod (vis = \"pub\"))] pub fn new"));
        assert!(expanded.contains(
            "# [doc = r\" How far the ratio may go.\"] # [builder (default = Consts :: MAX_BAR_RATIO)] ratio : f64"
        ));
        assert!(expanded.contains(
            "let phase : Phase = Phase :: Idle ; \
             let count : usize = :: core :: default :: Default :: default () ; \
             Self { ratio , phase , count }"
        ));
        assert!(
            !expanded.contains("phase : Phase)"),
            "a skipped field is no argument"
        );
    }

    #[kithara::test(native, flash(false))]
    fn a_document_config_can_keep_serde_construction_without_a_builder() {
        let expanded = expansion(quote! {
            #[config(builder(none))]
            pub struct Settings {
                #[config(value)]
                threshold: u32,
            }
        });

        assert!(!expanded.contains("bon :: bon"));
        assert!(expanded.contains("impl :: kithara_config :: Config for Settings"));
        assert!(expanded.contains("Clone :: clone (& self . threshold)"));
    }

    #[kithara::test(native, flash(false))]
    fn accessors_return_a_reference_or_a_copy() {
        let expanded = expansion(quote! {
            pub(crate) struct Settings {
                #[config(value, get(ref))]
                name: String,
                #[config(value, get(copy))]
                ratio: f64,
            }
        });

        assert!(expanded.contains("pub (crate) fn name (& self) -> & String { & self . name }"));
        assert!(expanded.contains("pub (crate) fn ratio (& self) -> f64 { self . ratio }"));
    }

    #[kithara::test(native, flash(false))]
    fn field_overrides_disable_inherited_getters() {
        let expanded = expansion(quote! {
            #[config(owner_access, fields(value, get(copy), live))]
            struct Settings {
                threshold: u32,
                #[config(get(skip))]
                ratio: u32,
            }
        });
        assert!(expanded.contains("fn threshold (& self) -> u32"));
        assert!(expanded.contains("Threshold (u32)"));
        assert!(
            expanded.contains("Ratio (u32)"),
            "the inherited live role stays"
        );
        assert!(!expanded.contains("fn ratio"));
        assert_eq!(
            refusal(quote! {
                #[config(owner_access, fields(value, get(skip)))]
                struct Settings { threshold: u32 }
            }),
            "owner_access requires a get(ref) or get(copy) accessor"
        );
    }

    #[kithara::test(native)]
    fn owner_access_refuses_a_getter_named_like_the_owners_own() {
        for input in [
            quote! {
                #[config(owner_access, fields(value, get(copy)))]
                struct Settings { config: u32 }
            },
            quote! {
                #[config(owner_access, fields(value, get(copy)))]
                struct Settings { level: u32, r#config: u32 }
            },
        ] {
            assert_eq!(
                refusal(input),
                "owner_access cannot generate a getter named config, which ConfigOwner declares"
            );
        }
    }

    #[kithara::test(native, flash(false))]
    fn shared_field_grammar_rejects_conflicts_at_both_scopes() {
        for options in [
            quote!(value, nested),
            quote!(value, get(ref), get(copy)),
            quote!(value, get(clone)),
            quote!(value, live, live),
            quote!(value, live, live(owner)),
            quote!(value, check = first, check = second),
            quote!(value, builder(default), builder(required)),
            quote!(value, patch(skip), patch(nested)),
            quote!(value, debug(skip), debug(skip)),
            quote!(value, skip = ""),
        ] {
            for input in [
                quote!(
                    #[config(debug, fields(#options))]
                    struct Settings {
                        field: u32,
                    }
                ),
                quote!(
                    #[config(debug)]
                    struct Settings {
                        #[config(#options)]
                        field: u32,
                    }
                ),
            ] {
                assert!(
                    expand(input).is_err(),
                    "conflicting options accepted: {options}"
                );
            }
        }
    }

    #[kithara::test(native, flash(false))]
    fn debug_omits_skipped_fields_and_bounds_generic_field_types() {
        let expanded = expansion(quote! {
            #[config(debug)]
            struct Player<S> {
                #[config(skip = "injected worker")]
                worker: Worker<S>,
                #[config(skip = "injected bus", debug(skip))]
                bus: Bus,
                #[config(value)]
                volume: f32,
            }
        });

        assert!(expanded.contains("Worker < S > : :: core :: fmt :: Debug"));
        assert!(!expanded.contains("S : :: core :: fmt :: Debug"));
        assert!(expanded.contains(". field (:: core :: stringify ! (volume) , & self . volume)"));
        assert!(!expanded.contains("stringify ! (bus)"));
        assert!(expanded.contains(". finish_non_exhaustive ()"));
    }

    #[kithara::test(native, flash(false))]
    fn a_wrapped_field_takes_its_wire_value_through_the_builder() {
        let expanded = expansion(quote! {
            struct Settings {
                #[config(value(f32, self.fade.load()), wrap(default = 1.5, with = Atomic::new))]
                fade: Atomic,
            }
        });

        assert!(expanded.contains(
            "# [builder (default = Atomic :: new (1.5) , with = | value : f32 | Atomic :: new (value))] fade : Atomic"
        ));
    }

    #[kithara::test(native, flash(false))]
    fn construction_inputs_have_a_builder_without_a_retained_snapshot() {
        let expanded = expansion(quote! {
            #[config(construction)]
            struct Input<T> {
                #[config(skip = "injected resource", builder(start_fn))]
                resource: T,
                #[config(value, builder(default), get(copy))]
                capacity: usize,
            }
        });

        assert!(expanded.contains("# [builder (start_fn)] resource : T"));
        assert!(expanded.contains("fn capacity (& self) -> usize"));
        assert!(!expanded.contains("InputValues"));
        assert!(!expanded.contains("kithara_config :: Config for Input"));
    }

    #[kithara::test(native, flash(false))]
    fn construction_inputs_reject_retained_only_options() {
        for options in [
            quote!(construction, default),
            quote!(construction, check(error = Error)),
            quote!(construction, sdk),
            quote!(construction, values_vis = "pub"),
        ] {
            assert!(
                expand(quote! {
                    #[config(#options)]
                    struct Input {
                        #[config(value)]
                        capacity: usize,
                    }
                })
                .is_err(),
                "construction accepted retained-only options: {options}"
            );
        }
    }

    #[kithara::test(native, flash(false))]
    fn validated_builders_require_a_declared_domain_check() {
        assert_eq!(
            refusal(quote! {
                #[config(validate_builder)]
                struct Settings {
                    #[config(value)]
                    limit: usize,
                }
            }),
            "validate_builder requires check(error = ...) or patch(validate = ..., error = ...)"
        );
        assert!(
            expansion(quote! {
                #[config(validate_builder, check(error = Error))]
                struct Settings {
                    #[config(value, check = Self::bounded)]
                    limit: usize,
                }
            })
            .contains("-> :: core :: result :: Result < Self , Error > { :: kithara_config :: CheckedConfig :: validated (Self { limit }) }"),
            "the field checks gate the builder"
        );
        assert_eq!(
            refusal(quote! {
                #[config(builder(existing), validate_builder, patch(validate = Self::check, error = Error))]
                struct Settings {
                    #[config(value)]
                    limit: usize,
                }
            }),
            "validate_builder requires a generated builder"
        );
    }

    #[kithara::test(native, flash(false))]
    fn malformed_field_declarations_are_rejected() {
        for field in [
            quote!(#[config(value, builder(default), builder(required))] field: u32),
            quote!(#[config(value, builder())] field: u32),
            quote!(#[config(value, builder(skip, default))] field: u32),
            quote!(#[config(value, field)] field: u32),
            quote!(#[config(value, field(get))] field: u32),
            quote!(#[config(value, field(set))] field: u32),
            quote!(#[config(value, field(copy))] field: u32),
            quote!(#[config(value, debug(show))] field: u32),
            quote!(#[config(value(u32, self.field.0), wrap(default = 1))] field: Wrapped),
            quote!(#[config(value, wrap(default = 1, with = Wrapped::new))] field: Wrapped),
            quote!(#[config(value(u32, self.field.0), wrap(default = 1, with = Wrapped::new), builder(required))] field: Wrapped),
            quote!(field: u32),
        ] {
            assert!(
                expand(quote!(#[config(debug)] struct Settings { #field })).is_err(),
                "malformed field declaration was accepted: {field}"
            );
        }
    }

    #[kithara::test(native, flash(false))]
    fn debug_skip_requires_a_generated_debug() {
        assert_eq!(
            refusal(quote! {
                struct Settings {
                    #[config(skip = "injected bus", debug(skip))]
                    bus: Bus,
                }
            }),
            "debug(skip) requires `#[config(debug)]` on the type"
        );
    }

    #[kithara::test(native, flash(false))]
    fn sdk_field_limit_requires_positive_value_role() {
        let projected = expansion(quote! {
            struct Source {
                #[config(value(u32, self.capacity.load()), sdk)]
                capacity: LiveU32,
            }
        });
        assert!(!projected.contains("sdk"));

        for field in [
            quote!(#[config(value, sdk(max = 0))] capacity: usize),
            quote!(#[config(value, sdk(max = 64), sdk(max = 128))] capacity: usize),
            quote!(#[config(skip = "resource", sdk(max = 64))] capacity: usize),
        ] {
            assert!(
                expand(quote!(struct Source { #field })).is_err(),
                "invalid SDK field declaration was accepted: {field}"
            );
        }
    }

    #[kithara::test(native)]
    fn field_checks_need_the_struct_error_and_a_value_field() {
        assert_eq!(
            refusal(quote! {
                struct Settings {
                    #[config(value, check = Self::bounded)]
                    level: u32,
                }
            }),
            "check = ... requires check(error = ...) on the struct"
        );
        for field in [
            quote!(#[config(nested, check = Self::bounded)] inner: Inner),
            quote!(#[config(skip = "resource", check = Self::bounded)] inner: Inner),
            quote!(#[config(value(u32, self.inner.get()), check = Self::bounded)] inner: Inner),
        ] {
            assert_eq!(
                refusal(quote!(#[config(check(error = Error))] struct Settings { #field })),
                "check requires a value field",
                "{field}"
            );
        }
        assert_eq!(
            refusal(quote! {
                #[config(check(error = Error), patch(validate = Self::validated, error = Error))]
                struct Settings {
                    #[config(value)]
                    level: u32,
                }
            }),
            "check(error = ...) and patch(validate = ..., error = ...) exclude each other"
        );
    }

    #[kithara::test(native)]
    fn live_fields_are_value_or_nested_fields_of_a_field_checked_struct() {
        for (input, message) in [
            (
                quote!(
                    struct S {
                        #[config(skip = "resource", live)]
                        handle: Handle,
                    }
                ),
                "live requires a value or nested field",
            ),
            (
                quote!(
                    struct S {
                        #[config(value(u32, self.handle.get()), live)]
                        handle: Handle,
                    }
                ),
                "live requires a value or nested field",
            ),
            (
                quote!(
                    struct S {
                        #[config(nested, live(owner))]
                        inner: Inner,
                    }
                ),
                "live(owner) requires a value field",
            ),
            (
                quote!(
                    struct S {
                        #[config(value, live(shared))]
                        level: u32,
                    }
                ),
                "expected live or live(owner)",
            ),
            (
                quote! {
                    #[config(patch(validate = Self::validated, error = Error))]
                    struct S { #[config(value, live)] level: u32 }
                },
                "live fields require check(error = ...) instead of patch(validate = ...)",
            ),
            (
                quote!(
                    struct S {
                        #[config(value, live)]
                        settings: u32,
                    }
                ),
                "a live configuration cannot name a field settings or configure",
            ),
            (
                quote!(
                    struct S {
                        #[config(value, live)]
                        level: u32,
                        #[config(value)]
                        configure: u32,
                    }
                ),
                "a live configuration cannot name a field settings or configure",
            ),
            (
                quote!(
                    struct S<T> {
                        #[config(value, live)]
                        level: u32,
                        #[config(skip = "resource")]
                        handle: T,
                    }
                ),
                "live fields require a configuration without generics",
            ),
            (
                quote!(
                    struct S {
                        #[config(value, live)]
                        _1: u32,
                    }
                ),
                "a live field needs a name that forms a change variant",
            ),
            (
                quote!(
                    struct S {
                        #[config(value, live)]
                        foo_bar: u32,
                        #[config(value, live)]
                        foo__bar: u32,
                    }
                ),
                "live fields `foo_bar` and `foo__bar` name the same change variant `FooBar`",
            ),
            (
                quote!(
                    struct S {
                        #[config(nested, live)]
                        left: Pan,
                        #[config(nested, live)]
                        right: Pan,
                    }
                ),
                "nested live fields `left` and `right` share a type, so its change cannot name the field",
            ),
            (
                quote!(
                    struct S {
                        #[config(value, live)]
                        gain: u32,
                        #[config(value, get(copy))]
                        set_gain: u32,
                    }
                ),
                "fields `gain` and `set_gain` both generate the method `set_gain`",
            ),
            (
                quote!(
                    struct S {
                        #[config(value, live(owner))]
                        live: u32,
                        #[config(value, live)]
                        beat: u32,
                    }
                ),
                "fields `live` and `beat` both generate the method `exec_live`",
            ),
            (
                quote!(
                    struct S {
                        #[config(value, live)]
                        r#settings: u32,
                    }
                ),
                "a live configuration cannot name a field settings or configure",
            ),
            (
                quote!(
                    struct S {
                        #[config(value, live)]
                        level: u32,
                        #[config(value, get(copy))]
                        r#configure: u32,
                    }
                ),
                "a live configuration cannot name a field settings or configure",
            ),
            (
                quote!(
                    #[config(construction)]
                    struct S {
                        #[config(value, live)]
                        level: u32,
                    }
                ),
                "construction inputs cannot declare live fields or checks",
            ),
        ] {
            assert_eq!(refusal(input), message);
        }
    }

    #[kithara::test(native)]
    fn a_field_checked_struct_validates_fields_in_order_and_nested_configs_whole() {
        let expanded = expansion(quote! {
            #[config(check(error = Error), fields(value))]
            pub struct Rig {
                #[config(check = Self::level_bounds)]
                level: u8,
                #[config(nested)]
                gauge: Gauge,
                #[config(check = Self::limit_bounds)]
                limit: u8,
            }
        });
        assert!(expanded.contains(
            "impl :: kithara_config :: CheckedConfig for Rig { type Error = Error ; \
             fn validated (mut self) -> :: core :: result :: Result < Self , Self :: Error > { \
             self . level = Self :: level_bounds (self . level) ? ; \
             self . gauge = :: kithara_config :: CheckedConfig :: validated (self . gauge) ? ; \
             self . limit = Self :: limit_bounds (self . limit) ? ; \
             :: core :: result :: Result :: Ok (self) } }"
        ));
        assert!(!expanded.contains("LiveConfig"), "no live field, no change");
        assert!(
            !expansion(quote!(
                struct Plain {
                    #[config(value)]
                    level: u8,
                }
            ))
            .contains("CheckedConfig")
        );
    }

    #[kithara::test(native)]
    fn a_live_field_is_one_variant_its_check_guards_and_its_assignment_applies() {
        let expanded = expansion(quote! {
            #[config(check(error = Error), fields(value))]
            pub struct Rig {
                #[config(live, check = Self::level_bounds)]
                level: u8,
                #[config(nested, live)]
                gauge: Gauge,
                #[config(live(owner))]
                rate: u32,
                limit: u8,
            }
        });
        assert!(expanded.contains(
            "pub enum RigChange { Level (u8) , \
             Gauge (< Gauge as :: kithara_config :: LiveConfig > :: Change) , Rate (u32) , }"
        ));
        assert!(expanded.contains(
            "impl :: core :: convert :: From < < Gauge as :: kithara_config :: LiveConfig > :: Change > for RigChange"
        ));
        assert!(expanded.contains(
            "const _ : () = :: core :: assert ! (! < Gauge as :: kithara_config :: LiveConfig > :: OWNER_FIELDS"
        ));
        assert!(expanded.contains("const OWNER_FIELDS : bool = true ;"));
        assert!(expanded.contains(
            "RigChange :: Level (__kithara_value) => :: core :: result :: Result :: Ok (RigChange :: Level (Self :: level_bounds (__kithara_value) ?))"
        ));
        assert!(expanded.contains(
            "RigChange :: Gauge (__kithara_value) => :: core :: result :: Result :: Ok (RigChange :: Gauge (\
             < Gauge as :: kithara_config :: LiveConfig > :: check (__kithara_value) ?))"
        ));
        assert!(expanded.contains("RigChange :: Rate (__kithara_value) => :: core :: result :: Result :: Ok (RigChange :: Rate (__kithara_value))"));
        assert!(
            expanded
                .contains("RigChange :: Level (__kithara_value) => self . level = __kithara_value")
        );
        assert!(expanded.contains(
            "RigChange :: Gauge (__kithara_value) => :: kithara_config :: LiveConfig :: apply_change (& mut self . gauge , __kithara_value)"
        ));
        assert!(
            !expanded.contains("Limit ("),
            "a field without live has no variant"
        );
    }
}

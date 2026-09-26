use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::{Attribute, Fields, Item, ItemEnum, ItemStruct, Result, parse::Parser as _, parse_quote};

use super::field;

pub(crate) fn expand(attributes: TokenStream, input: TokenStream) -> Result<TokenStream> {
    let item: Item = syn::parse2(input)?;
    match item {
        Item::Struct(item) => retained(attributes, item),
        Item::Enum(item) => construction_enum(attributes, item),
        Item::Impl(item) if attributes.is_empty() => Ok(quote! {
            #[::kithara_config::__private::bon::bon(crate = ::kithara_config::__private::bon)]
            #item
        }),
        Item::Fn(item) if attributes.is_empty() => Ok(quote! {
            #[::kithara_config::__private::bon::builder(crate = ::kithara_config::__private::bon)]
            #item
        }),
        Item::Fn(item) => delegated(attributes, &item),
        other => Err(syn::Error::new_spanned(
            other,
            "config requires a named struct, a construction enum, or a function/impl without options",
        )),
    }
}

fn construction_enum(options: TokenStream, mut item: ItemEnum) -> Result<TokenStream> {
    let mut construction = false;
    let mut builder_disabled = false;
    syn::meta::parser(|meta| {
        if meta.path.is_ident("construction") && !construction {
            construction = true;
        } else if meta.path.is_ident("builder") && !builder_disabled {
            let value: syn::LitBool = meta.value()?.parse()?;
            if value.value {
                return Err(meta.error("enum construction uses its existing builder"));
            }
            builder_disabled = true;
        } else {
            return Err(meta.error("expected construction, builder = false"));
        }
        Ok(())
    })
    .parse2(options)?;
    if !construction || !builder_disabled {
        return Err(syn::Error::new_spanned(
            &item.ident,
            "construction enum requires construction, builder = false",
        ));
    }
    for variant in &mut item.variants {
        if let Some(position) = variant
            .attrs
            .iter()
            .position(|attr| attr.path().is_ident("config"))
        {
            let attr = variant.attrs.remove(position);
            attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("sdk") {
                    Ok(())
                } else {
                    Err(meta.error("construction enum variant supports only sdk"))
                }
            })?;
        }
        let Fields::Named(fields) = &mut variant.fields else {
            return Err(syn::Error::new_spanned(
                variant,
                "construction enum variants require named fields",
            ));
        };
        for field in &mut fields.named {
            let position = field
                .attrs
                .iter()
                .position(|attr| attr.path().is_ident("config"));
            let Some(position) = position else {
                return Err(syn::Error::new_spanned(
                    field,
                    "config field is unclassified",
                ));
            };
            let attr = field.attrs.remove(position);
            let mut classified = false;
            attr.parse_nested_meta(|meta| {
                if classified {
                    return Err(meta.error("config enum field requires exactly one role"));
                }
                classified = true;
                if meta.path.is_ident("value") || meta.path.is_ident("nested") {
                    Ok(())
                } else if meta.path.is_ident("skip") {
                    let reason: syn::LitStr = meta.value()?.parse()?;
                    if reason.value().trim().is_empty() {
                        return Err(meta.error("skip requires a reason"));
                    }
                    Ok(())
                } else {
                    Err(meta.error("expected value, nested, or skip = reason"))
                }
            })?;
            if !classified {
                return Err(syn::Error::new_spanned(
                    field,
                    "config enum field requires one role",
                ));
            }
        }
    }
    Ok(quote!(#item))
}

fn delegated(options: TokenStream, item: &syn::ItemFn) -> Result<TokenStream> {
    let mut property = None;
    let mut sdk = false;
    let mut seen: Vec<syn::Path> = Vec::new();
    syn::meta::parser(|meta| {
        if seen.contains(&meta.path) {
            return Err(meta.error("duplicate config option"));
        }
        seen.push(meta.path.clone());
        if meta.path.is_ident("delegate") {
            let value: syn::LitStr = meta.value()?.parse()?;
            if value.value().trim().is_empty() {
                return Err(meta.error("delegated property name cannot be empty"));
            }
            property = Some(value);
        } else if meta.path.is_ident("sdk") {
            sdk = true;
        } else {
            return Err(meta.error("expected delegate = property or sdk"));
        }
        Ok(())
    })
    .parse2(options)?;
    if property.is_none() {
        return Err(syn::Error::new_spanned(
            &item.sig.ident,
            "config operation requires delegate = property",
        ));
    }
    if !sdk {
        return Err(syn::Error::new_spanned(
            &item.sig.ident,
            "delegated config operation requires explicit sdk exposure",
        ));
    }
    Ok(quote!(#item))
}

fn retained(options: TokenStream, mut item: ItemStruct) -> Result<TokenStream> {
    let mut built_default = false;
    let mut builder = true;
    let mut construction = false;
    let mut runtime_update = false;
    let mut sdk = false;
    let mut values_vis = None;
    let mut seen: Vec<syn::Path> = Vec::new();
    syn::meta::parser(|meta| {
        if seen.contains(&meta.path) {
            return Err(meta.error("duplicate config option"));
        }
        seen.push(meta.path.clone());
        if meta.path.is_ident("default") {
            built_default = true;
        } else if meta.path.is_ident("construction") {
            construction = true;
        } else if meta.path.is_ident("update") {
            runtime_update = true;
        } else if meta.path.is_ident("sdk") {
            sdk = true;
        } else if meta.path.is_ident("builder") {
            builder = meta.value()?.parse::<syn::LitBool>()?.value;
        } else if meta.path.is_ident("values_vis") {
            let visibility: syn::LitStr = meta.value()?.parse()?;
            values_vis = Some(syn::parse_str::<syn::Visibility>(&visibility.value())?);
        } else {
            return Err(meta.error(
                "expected construction, default, update, sdk, builder = false, or values_vis",
            ));
        }
        Ok(())
    })
    .parse2(options)?;
    if built_default && !builder {
        return Err(syn::Error::new_spanned(
            &item.ident,
            "default requires the struct builder",
        ));
    }
    if construction && (built_default || runtime_update || sdk || values_vis.is_some()) {
        return Err(syn::Error::new_spanned(
            &item.ident,
            "construction inputs cannot declare retained defaults, updates, SDK records, or values visibility",
        ));
    }
    let Fields::Named(fields) = &mut item.fields else {
        return Err(syn::Error::new_spanned(
            item,
            "config requires named fields",
        ));
    };
    let mut value_fields: Vec<TokenStream> = Vec::new();
    let mut reads: Vec<TokenStream> = Vec::new();
    let mut update_declarations: Vec<TokenStream> = Vec::new();
    let mut update_fields: Vec<TokenStream> = Vec::new();
    let mut update_lowers: Vec<TokenStream> = Vec::new();
    let name = &item.ident;
    for member in &mut fields.named {
        if let Some(expanded) = field::expand(member, &item.generics, name, !construction)? {
            value_fields.push(expanded.declaration);
            reads.push(expanded.read);
            if let Some(update) = expanded.update {
                update_declarations.push(update.declaration);
                update_fields.push(update.field);
                update_lowers.push(update.lower);
            }
        }
    }
    let has_fieldwork_field = fields
        .named
        .iter()
        .any(|field| field.attrs.iter().any(|attr| attr.path().is_ident("field")));
    if !runtime_update && !update_fields.is_empty() {
        return Err(syn::Error::new_spanned(
            name,
            "field runtime updates require `#[config(update)]` on the struct",
        ));
    }
    if runtime_update && update_fields.is_empty() {
        return Err(syn::Error::new_spanned(
            name,
            "`#[config(update)]` requires at least one `#[config(value, update)]` field",
        ));
    }
    if runtime_update && !has_derive(&item.attrs, "Patch")? {
        return Err(syn::Error::new_spanned(
            name,
            "runtime updates require `#[derive(Patch)]` on the retained config",
        ));
    }
    let values = format_ident!("{name}Values");
    let visibility = values_vis.as_ref().unwrap_or(&item.vis);
    let gates = attributes(&item.attrs, false)?;
    let (impl_generics, ty_generics, where_clause) = item.generics.split_for_impl();
    let snapshot = if construction {
        TokenStream::new()
    } else {
        quote! {
            #(#gates)*
            #[doc = concat!("Owned readable values of `", stringify!(#name), "`. Resource inputs are excluded.")]
            #visibility struct #values {
                #(#value_fields,)*
            }
            #(#gates)*
            #[automatically_derived]
            impl #impl_generics ::kithara_config::Config for #name #ty_generics #where_clause {
                type Values = #values;
                fn values(&self) -> Self::Values {
                    #values { #(#reads,)* }
                }
            }
        }
    };
    let runtime = if runtime_update {
        runtime_updates(
            &item,
            visibility,
            &update_declarations,
            &update_fields,
            &update_lowers,
        )?
    } else {
        TokenStream::new()
    };
    if builder {
        item.attrs.insert(
            0,
            parse_quote!(#[derive(::kithara_config::__private::bon::Builder)]),
        );
        item.attrs
            .push(parse_quote!(#[builder(crate = ::kithara_config::__private::bon)]));
    }
    if built_default {
        item.attrs.insert(
            0,
            parse_quote!(#[derive(::kithara_config::__private::BuiltDefault)]),
        );
    }
    compose_fieldwork(&mut item, construction, has_fieldwork_field)?;
    Ok(quote! { #item #snapshot #runtime })
}

fn compose_fieldwork(
    item: &mut ItemStruct,
    construction: bool,
    has_fieldwork_field: bool,
) -> Result<()> {
    let fieldwork = !construction
        || has_derive(&item.attrs, "Fieldwork")?
        || item
            .attrs
            .iter()
            .any(|attr| attr.path().is_ident("fieldwork"))
        || has_fieldwork_field;
    if fieldwork {
        if !item
            .attrs
            .iter()
            .any(|attr| attr.path().is_ident("fieldwork"))
        {
            item.attrs.push(parse_quote!(#[fieldwork(opt_in, get)]));
        }
        if !has_derive(&item.attrs, "Fieldwork")? {
            item.attrs.insert(
                0,
                parse_quote!(#[derive(::kithara_config::__private::Fieldwork)]),
            );
        }
    }
    Ok(())
}

fn has_derive(attributes: &[Attribute], expected: &str) -> Result<bool> {
    for attribute in attributes
        .iter()
        .filter(|attr| attr.path().is_ident("derive"))
    {
        let paths = attribute.parse_args_with(
            syn::punctuated::Punctuated::<syn::Path, syn::Token![,]>::parse_terminated,
        )?;
        if paths.iter().any(|path| {
            path.segments
                .last()
                .is_some_and(|segment| segment.ident == expected)
        }) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn runtime_updates(
    item: &ItemStruct,
    visibility: &syn::Visibility,
    declarations: &[TokenStream],
    fields: &[TokenStream],
    lowers: &[TokenStream],
) -> Result<TokenStream> {
    let name = &item.ident;
    let update = format_ident!("{name}Update");
    let patch = format_ident!("{name}Patch");
    let error = format_ident!("{name}PatchError");
    let (impl_generics, ty_generics, where_clause) = item.generics.split_for_impl();
    let gates = attributes(&item.attrs, false)?;
    #[cfg(feature = "patch")]
    let fallible = crate::config::patch::is_fallible(&item.attrs, name.span())?;
    #[cfg(not(feature = "patch"))]
    let fallible = {
        return Err(syn::Error::new_spanned(
            name,
            "runtime updates require the kithara-derive `patch` feature",
        ));
    };
    let apply = if fallible {
        quote! {
            #visibility fn apply_update(
                &mut self,
                update: #update,
            ) -> ::core::result::Result<(), #error> {
                let mut patch = #patch::default();
                #(#lowers)*
                self.apply(patch)
            }
        }
    } else {
        quote! {
            #visibility fn apply_update(&mut self, update: #update) {
                let mut patch = #patch::default();
                #(#lowers)*
                self.apply(patch);
            }
        }
    };
    Ok(quote! {
        #(#declarations)*
        #(#gates)*
        #[derive(::core::default::Default)]
        #[non_exhaustive]
        #visibility struct #update {
            #(#fields,)*
        }
        #(#gates)*
        #[automatically_derived]
        impl #impl_generics #name #ty_generics #where_clause {
            #apply
        }
    })
}

pub(super) fn attributes(input: &[Attribute], docs: bool) -> Result<Vec<Attribute>> {
    input
        .iter()
        .filter_map(|attr| match filter_meta(&attr.meta, docs) {
            Ok(Some(meta)) => Some(Ok(parse_quote!(#[#meta]))),
            Ok(None) => None,
            Err(error) => Some(Err(error)),
        })
        .collect()
}

fn filter_meta(meta: &syn::Meta, docs: bool) -> Result<Option<syn::Meta>> {
    if meta.path().is_ident("cfg") || (docs && meta.path().is_ident("doc")) {
        return Ok(Some(meta.clone()));
    }
    if let syn::Meta::List(list) = meta
        && list.path.is_ident("cfg_attr")
    {
        let arguments = list.parse_args_with(
            syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
        )?;
        let mut arguments = arguments.iter();
        let condition = arguments
            .next()
            .ok_or_else(|| syn::Error::new_spanned(list, "missing cfg_attr condition"))?;
        let mut kept: Vec<syn::Meta> = Vec::new();
        for argument in arguments {
            if let Some(meta) = filter_meta(argument, docs)? {
                kept.push(meta);
            }
        }
        if !kept.is_empty() {
            return Ok(Some(parse_quote!(cfg_attr(#condition, #(#kept),*))));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;
    use quote::quote;

    use super::expand;

    #[kithara::test(native, flash(false))]
    fn duplicate_options_are_rejected_instead_of_overriding_configuration() {
        for options in [
            quote!(default, default),
            quote!(builder = false, builder = true),
            quote!(values_vis = "pub", values_vis = "pub(crate)"),
        ] {
            let error = expand(
                options,
                quote! {
                    struct Settings {
                        #[config(value)]
                        threshold: u32,
                    }
                },
            )
            .expect_err("duplicate configuration options must be rejected");
            assert_eq!(error.to_string(), "duplicate config option");
        }
    }

    #[kithara::test(native, flash(false))]
    fn field_groups_forward_to_existing_derives_without_changing_value_role() {
        let expanded = expand(
            quote!(builder = false),
            quote! {
                struct Settings {
                    #[config(value, builder(default = Consts::MAX_BAR_RATIO), field(get, copy), patch(skip))]
                    ratio: f64,
                }
            },
        )
        .expect("independent field namespaces are accepted")
        .to_string();

        assert!(expanded.contains("builder (default = Consts :: MAX_BAR_RATIO)"));
        assert!(expanded.contains("field (get , copy)"));
        assert!(expanded.contains("patch (skip)"));
        assert!(expanded.contains("pub ratio : f64"));
    }

    #[kithara::test(native, flash(false))]
    fn construction_inputs_keep_field_attributes_without_a_retained_snapshot() {
        let expanded = expand(
            quote!(construction, builder = false),
            quote! {
                struct Input<T> {
                    #[config(skip = "injected resource", builder(start_fn), patch(skip))]
                    resource: T,
                    #[config(value, builder(default), field(get, copy))]
                    capacity: usize,
                }
            },
        )
        .expect("construction inputs are classified without retaining resources")
        .to_string();

        assert!(expanded.contains("builder (start_fn)"));
        assert!(expanded.contains("patch (skip)"));
        assert!(expanded.contains("field (get , copy)"));
        assert!(!expanded.contains("InputValues"));
        assert!(!expanded.contains("kithara_config :: Config for Input"));
    }

    #[kithara::test(native, flash(false))]
    fn construction_inputs_reject_retained_only_options() {
        for options in [
            quote!(construction, default),
            quote!(construction, update),
            quote!(construction, sdk),
            quote!(construction, values_vis = "pub"),
        ] {
            assert!(
                expand(
                    options.clone(),
                    quote! {
                        struct Input {
                            #[config(value)]
                            capacity: usize,
                        }
                    },
                )
                .is_err(),
                "construction accepted retained-only options: {options}"
            );
        }
    }

    #[kithara::test(native, flash(false))]
    fn construction_without_accessor_fields_does_not_add_fieldwork() {
        let expanded = expand(
            quote!(construction, builder = false),
            quote! {
                #[derive(Builder, Patch)]
                struct Input<T> {
                    #[config(skip = "injected resource", builder(start_fn), patch(skip))]
                    resource: T,
                    #[config(value)]
                    capacity: usize,
                }
            },
        )
        .expect("a consumed input needs no generated accessors")
        .to_string();
        assert!(!expanded.contains("Fieldwork"));
        assert!(!expanded.contains("fieldwork"));
    }

    #[kithara::test(native, flash(false))]
    fn duplicate_native_and_wrapped_field_namespaces_are_rejected() {
        for (native, wrapped) in [
            (quote!(#[builder(default)]), quote!(builder(default))),
            (quote!(#[field(get, copy)]), quote!(field(get, copy))),
            (quote!(#[patch(skip)]), quote!(patch(skip))),
        ] {
            let error = expand(
                quote!(builder = false),
                quote! {
                    struct Settings {
                        #native
                        #[config(value, #wrapped)]
                        field: u32,
                    }
                },
            )
            .expect_err("one field namespace cannot have two owners");
            assert!(
                error
                    .to_string()
                    .contains("choose either a native field attribute or its config group"),
                "{error}"
            );
        }
    }

    #[kithara::test(native, flash(false))]
    fn duplicate_or_malformed_field_groups_are_rejected() {
        for group in [
            quote!(builder(default), builder(required)),
            quote!(builder()),
            quote!(field),
            quote!(patch(skip =)),
        ] {
            assert!(
                expand(
                    quote!(builder = false),
                    quote! {
                        struct Settings {
                            #[config(value, #group)]
                            field: u32,
                        }
                    },
                )
                .is_err(),
                "malformed field group was accepted: {group}"
            );
        }
    }

    #[kithara::test(native, flash(false))]
    fn sdk_field_limit_requires_positive_value_role() {
        let accepted = expand(
            quote!(construction, builder = false),
            quote! {
                struct Source {
                    #[config(value, sdk(max = 64))]
                    capacity: usize,
                }
            },
        )
        .expect("bounded SDK value is accepted")
        .to_string();
        assert!(!accepted.contains("sdk"));

        let projected = expand(
            quote!(builder = false),
            quote! {
                struct Source {
                    #[config(value(u32, self.capacity.load()), sdk)]
                    capacity: LiveU32,
                }
            },
        )
        .expect("an SDK projected value does not need a numeric maximum")
        .to_string();
        assert!(!projected.contains("sdk"));

        for field in [
            quote!(#[config(value, sdk(max = 0))] capacity: usize),
            quote!(#[config(value, sdk(max = 64), sdk(max = 128))] capacity: usize),
            quote!(#[config(skip = "resource", sdk(max = 64))] capacity: usize),
        ] {
            assert!(
                expand(
                    quote!(construction, builder = false),
                    quote!(struct Source { #field })
                )
                .is_err(),
                "invalid SDK field declaration was accepted: {field}"
            );
        }
    }

    #[kithara::test(native, flash(false))]
    fn runtime_update_cannot_lower_through_a_skipped_patch_field() {
        for declaration in [
            quote!(#[config(value, update, patch(skip))]),
            quote!(#[patch(skip)] #[config(value, update)]),
        ] {
            let error = expand(
                quote!(builder = false, update),
                quote! {
                    #[derive(Patch)]
                    struct Settings {
                        #declaration
                        value: u32,
                    }
                },
            )
            .expect_err("the update target must exist in Patch");
            assert!(
                error
                    .to_string()
                    .contains("runtime update cannot use patch(skip)"),
                "{error}"
            );
        }
    }

    #[kithara::test(native, flash(false))]
    fn delegated_operations_require_a_named_sdk_property() {
        let expanded = expand(
            quote!(delegate = "eq_layout", sdk),
            quote! {
                pub fn set_eq_layout(&self, layout: Vec<EqBandConfig>) -> Result<(), Error> {
                    self.runtime.set_eq_layout(layout)
                }
            },
        )
        .expect("valid delegated operation")
        .to_string();
        assert!(expanded.contains("fn set_eq_layout"));
        assert!(!expanded.contains("delegate"));

        for (options, expected) in [
            (quote!(sdk), "config operation requires delegate = property"),
            (
                quote!(delegate = "eq_layout"),
                "delegated config operation requires explicit sdk exposure",
            ),
        ] {
            assert_eq!(
                expand(
                    options,
                    quote!(
                        fn update(&self) {}
                    )
                )
                .expect_err("incomplete operation metadata must fail")
                .to_string(),
                expected
            );
        }
    }

    #[kithara::test(native, flash(false))]
    #[cfg(feature = "patch")]
    fn optional_updates_emit_clear_and_only_declared_defaults_emit_reset() {
        let expanded = expand(
            quote!(default, update),
            quote! {
                #[derive(Clone, Patch)]
                struct Settings {
                    #[config(value, update, builder(default = Some(3)))]
                    width: Option<usize>,
                    #[config(value, update)]
                    required: usize,
                }
            },
        )
        .expect("valid runtime update declaration")
        .to_string();

        assert!(expanded.contains("enum SettingsWidthUpdate"));
        assert!(expanded.contains("Clear"));
        assert!(expanded.contains("Reset"));
        assert!(expanded.contains("enum SettingsRequiredUpdate"));
        assert_eq!(
            expanded.matches("Reset").count(),
            2,
            "one variant and one lowering arm"
        );
        assert_eq!(
            expanded.matches("Clear").count(),
            2,
            "one variant and one lowering arm"
        );
    }

    #[kithara::test(native, flash(false))]
    #[cfg(feature = "patch")]
    fn update_rejects_non_value_roles_and_missing_struct_opt_in() {
        let nested = expand(
            quote!(update),
            quote! {
                struct Settings {
                    #[config(nested, update)]
                    nested: Nested,
                }
            },
        )
        .expect_err("nested updates need their own declared operation");
        assert_eq!(
            nested.to_string(),
            "runtime update currently requires a retained value field"
        );

        let missing = expand(
            quote!(),
            quote! {
                struct Settings {
                    #[config(value, update)]
                    value: usize,
                }
            },
        )
        .expect_err("field update requires struct opt-in");
        assert_eq!(
            missing.to_string(),
            "field runtime updates require `#[config(update)]` on the struct"
        );

        let patch = expand(
            quote!(update),
            quote! {
                struct Settings {
                    #[config(value, update)]
                    value: usize,
                }
            },
        )
        .expect_err("runtime lowering requires the existing Patch owner");
        assert_eq!(
            patch.to_string(),
            "runtime updates require `#[derive(Patch)]` on the retained config"
        );
    }
}

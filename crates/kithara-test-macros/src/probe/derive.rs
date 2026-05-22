use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{Data, DataStruct, DeriveInput, Error, Field, Fields, Ident, LitStr};

#[derive(Default)]
struct FieldOpts {
    rename: Option<String>,
    skip: bool,
}

fn parse_field_opts(field: &Field) -> syn::Result<FieldOpts> {
    let mut opts = FieldOpts::default();
    for attr in &field.attrs {
        if !attr.path().is_ident("probe") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("skip") {
                opts.skip = true;
                Ok(())
            } else if meta.path.is_ident("name") {
                let lit: LitStr = meta.value()?.parse()?;
                opts.rename = Some(lit.value());
                Ok(())
            } else {
                Err(meta.error("unknown #[probe(...)] field option (expected `skip` or `name`)"))
            }
        })?;
    }
    Ok(opts)
}

pub(crate) fn expand_derive(input: &DeriveInput) -> syn::Result<TokenStream2> {
    let struct_name = &input.ident;

    let crate_name = std::env::var("CARGO_PKG_NAME")
        .map_err(|_| {
            Error::new_spanned(
                struct_name,
                "#[derive(Probe)] requires CARGO_PKG_NAME env var (set automatically by cargo)",
            )
        })?
        .replace('-', "_");
    let target = format!("{crate_name}_probe");

    let fields = match &input.data {
        Data::Struct(DataStruct {
            fields: Fields::Named(named),
            ..
        }) => &named.named,
        Data::Struct(_) => {
            return Err(Error::new_spanned(
                struct_name,
                "#[derive(Probe)] requires a struct with named fields",
            ));
        }
        _ => {
            return Err(Error::new_spanned(
                struct_name,
                "#[derive(Probe)] is only supported on structs",
            ));
        }
    };

    let mut field_idents: Vec<Ident> = Vec::new();
    let mut wire_names: Vec<String> = Vec::new();
    for field in fields {
        let opts = parse_field_opts(field)?;
        if opts.skip {
            continue;
        }
        let ident = field
            .ident
            .clone()
            .ok_or_else(|| Error::new_spanned(field, "expected named field"))?;
        let wire = opts.rename.unwrap_or_else(|| ident.to_string());
        field_idents.push(ident);
        wire_names.push(wire);
    }

    if field_idents.len() > 6 {
        return Err(Error::new_spanned(
            struct_name,
            "#[derive(Probe)] supports at most 6 wire fields (USDT \
             provider arity ceiling). Mark extra fields with `#[probe(skip)]` \
             or split the struct.",
        ));
    }
    let fire_fn = format_ident!("fire_{}", field_idents.len());

    let slot_idents: Vec<Ident> = (0..field_idents.len())
        .map(|i| format_ident!("__probe_slot_{}", i))
        .collect();

    let bindings: Vec<TokenStream2> = field_idents
        .iter()
        .zip(slot_idents.iter())
        .map(|(field, slot)| {
            quote! {
                let #slot: u64 = ::kithara_test_utils::probe::IntoProbeArg::into_probe_arg(self.#field);
            }
        })
        .collect();

    let tracing_pairs: Vec<TokenStream2> = wire_names
        .iter()
        .zip(slot_idents.iter())
        .map(|(name, slot)| {
            let ident = format_ident!("{}", name);
            quote! { #ident = #slot }
        })
        .collect();

    let field_consume: Vec<TokenStream2> = field_idents
        .iter()
        .map(|f| quote! { let _ = &self.#f; })
        .collect();

    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    Ok(quote! {
        impl #impl_generics ::kithara_test_utils::probe::Probe for #struct_name #ty_generics #where_clause {
            #[inline]
            fn record_probe(&self, name: &'static str) {
                let _ = name;
                #(#field_consume)*
                #[cfg(any(test, feature = "probe"))]
                {
                    ::kithara_test_utils::probe::register_probes();
                    #(#bindings)*
                    ::kithara_test_utils::probe::#fire_fn(name, #(#slot_idents),*);
                    ::tracing::event!(
                        target: #target,
                        ::tracing::Level::TRACE,
                        probe = name,
                        #(#tracing_pairs),*
                    );
                }
            }
        }
    })
}

/// Expand `#[derive(kithara::IntoProbeArg)]` for a single-field
/// `Copy` newtype struct. Generates round-trippable `into_probe_arg`
/// and `from_probe_arg` impls that delegate to the inner field's own
/// `IntoProbeArg` impl. Multi-field structs and enums are rejected:
/// they need an explicit packed impl with a documented bit layout
/// (`SegmentRequest::into_probe_arg` is the canonical example).
pub(crate) fn expand_derive_into_probe_arg(input: &DeriveInput) -> syn::Result<TokenStream2> {
    let struct_name = &input.ident;
    let Data::Struct(DataStruct { fields, .. }) = &input.data else {
        return Err(Error::new_spanned(
            struct_name,
            "#[derive(IntoProbeArg)] is only supported on structs",
        ));
    };

    let (field_access, field_ty, ctor): (TokenStream2, TokenStream2, TokenStream2) = match fields {
        Fields::Unit => {
            return Err(Error::new_spanned(
                struct_name,
                "#[derive(IntoProbeArg)] requires exactly one field — \
                 unit structs carry no probe payload",
            ));
        }
        Fields::Unnamed(unnamed) => {
            if unnamed.unnamed.len() != 1 {
                return Err(Error::new_spanned(
                    struct_name,
                    "#[derive(IntoProbeArg)] requires exactly one tuple field. \
                     Multi-field structs need an explicit packed impl with a \
                     documented bit layout (see `SegmentRequest`).",
                ));
            }
            let field = unnamed.unnamed.first().expect("checked len == 1");
            let ty = field.ty.clone();
            (
                quote!(self.0),
                quote!(#ty),
                quote!(Self(<#ty as ::kithara_test_utils::probe::IntoProbeArg>::from_probe_arg(packed))),
            )
        }
        Fields::Named(named) => {
            if named.named.len() != 1 {
                return Err(Error::new_spanned(
                    struct_name,
                    "#[derive(IntoProbeArg)] requires exactly one named field. \
                     Multi-field structs need an explicit packed impl with a \
                     documented bit layout (see `SegmentRequest`).",
                ));
            }
            let field = named.named.first().expect("checked len == 1");
            let name = field
                .ident
                .as_ref()
                .ok_or_else(|| Error::new_spanned(field, "expected named field"))?;
            let ty = field.ty.clone();
            (
                quote!(self.#name),
                quote!(#ty),
                quote!(Self {
                    #name: <#ty as ::kithara_test_utils::probe::IntoProbeArg>::from_probe_arg(packed),
                }),
            )
        }
    };

    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    Ok(quote! {
        impl #impl_generics ::kithara_test_utils::probe::IntoProbeArg
        for #struct_name #ty_generics #where_clause {
            fn into_probe_arg(self) -> u64 {
                <#field_ty as ::kithara_test_utils::probe::IntoProbeArg>::into_probe_arg(#field_access)
            }
            fn from_probe_arg(packed: u64) -> Self {
                #ctor
            }
        }
    })
}

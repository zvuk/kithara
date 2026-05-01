//! Expansion logic for `#[kithara::probe]` and `#[derive(Probe)]`.
//!
//! Kept out of `lib.rs` because workspace lint policy forbids non-entry
//! items (functions, structs, helpers) in `lib.rs` / `mod.rs` files.

use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{
    Data, DataStruct, DeriveInput, Field, Fields, FnArg, Ident, ItemFn, LitStr, Pat, PatIdent,
    Token, parse::Parser, punctuated::Punctuated,
};

pub(crate) fn parse_filter(attr: TokenStream2) -> syn::Result<Option<Vec<Ident>>> {
    if attr.is_empty() {
        return Ok(None);
    }
    let parser = Punctuated::<Ident, Token![,]>::parse_terminated;
    let parsed = parser.parse2(attr)?;
    Ok(Some(parsed.into_iter().collect()))
}

pub(crate) fn expand(input: &ItemFn, filter: Option<Vec<Ident>>) -> syn::Result<TokenStream2> {
    let fn_name = input.sig.ident.clone();
    let fn_name_str = fn_name.to_string();
    let probe_mod = format_ident!("__kithara_probe_{}", fn_name);

    let crate_name = std::env::var("CARGO_PKG_NAME")
        .map_err(|_| {
            syn::Error::new_spanned(
                &input.sig.ident,
                "#[kithara::probe] requires CARGO_PKG_NAME env var (set automatically by cargo)",
            )
        })?
        .replace('-', "_");
    let provider = crate_name.clone();
    let target = format!("{crate_name}_probe");

    let mut all_args: Vec<Ident> = Vec::new();
    for arg in &input.sig.inputs {
        match arg {
            FnArg::Receiver(_) => {
                // `self` / `&self` / `&mut self` is allowed — it's
                // simply not eligible to participate in the probe wire.
            }
            FnArg::Typed(typed) => match typed.pat.as_ref() {
                Pat::Ident(PatIdent { ident, .. }) => all_args.push(ident.clone()),
                other => {
                    return Err(syn::Error::new_spanned(
                        other,
                        "#[kithara::probe] requires plain named arguments (no patterns)",
                    ));
                }
            },
        }
    }

    let arg_idents: Vec<Ident> = match filter {
        None => all_args,
        Some(names) => {
            for name in &names {
                if !all_args.iter().any(|a| a == name) {
                    return Err(syn::Error::new_spanned(
                        name,
                        format!(
                            "#[kithara::probe(...)] arg `{name}` does not match any function parameter"
                        ),
                    ));
                }
            }
            names
        }
    };

    let probe_idents: Vec<Ident> = (0..arg_idents.len())
        .map(|i| format_ident!("__probe_arg_{}", i))
        .collect();

    let arg_bindings: Vec<TokenStream2> = arg_idents
        .iter()
        .zip(probe_idents.iter())
        .map(|(arg, slot)| {
            quote! {
                #[cfg(feature = "usdt-probes")]
                let #slot: u64 = ::kithara_probes::IntoProbeArg::into_probe_arg(#arg);
            }
        })
        .collect();

    // Suppress unused-variable warnings on probe args when the
    // `usdt-probes` feature is off (probe body becomes empty, args
    // appear unused). `let _ = &arg;` is a zero-codegen no-op the
    // compiler folds away.
    let arg_consume: Vec<TokenStream2> =
        arg_idents.iter().map(|a| quote! { let _ = &#a; }).collect();

    let usdt_provider_params: Vec<TokenStream2> = probe_idents
        .iter()
        .enumerate()
        .map(|(i, _)| {
            let p = format_ident!("_a{i}");
            quote! { #p: u64 }
        })
        .collect();

    let usdt_call_tuple = &probe_idents;
    let tracing_fields: Vec<TokenStream2> = arg_idents
        .iter()
        .zip(probe_idents.iter())
        .map(|(name, slot)| quote! { #name = #slot })
        .collect();

    let attrs = &input.attrs;
    let vis = &input.vis;
    let sig = &input.sig;
    let block = &input.block;

    Ok(quote! {
        #(#attrs)*
        #[cfg_attr(
            all(feature = "usdt-probes", not(target_arch = "wasm32")),
            expect(
                unsafe_code,
                clippy::items_after_statements,
                reason = "USDT inline asm + per-probe `static` items emitted inline by `usdt::provider!`"
            )
        )]
        #vis #sig {
            // The USDT provider module must be declared BEFORE any
            // statements; otherwise clippy's `items_after_statements`
            // fires on the macro expansion site.
            #[cfg(all(feature = "usdt-probes", not(target_arch = "wasm32")))]
            #[::usdt::provider(provider = #provider)]
            mod #probe_mod {
                pub(super) fn #fn_name( #(#usdt_provider_params),* ) {}
            }

            #(#arg_consume)*
            #(#arg_bindings)*

            #[cfg(all(feature = "usdt-probes", not(target_arch = "wasm32")))]
            #probe_mod::#fn_name!(|| ( #(#usdt_call_tuple),* ));

            #[cfg(feature = "usdt-probes")]
            ::tracing::event!(
                target: #target,
                ::tracing::Level::TRACE,
                probe = #fn_name_str,
                #(#tracing_fields),*
            );

            #block
        }
    })
}

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

fn camel_to_snake(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for (i, ch) in s.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

pub(crate) fn expand_derive(input: &DeriveInput) -> syn::Result<TokenStream2> {
    let struct_name = &input.ident;
    let snake = camel_to_snake(&struct_name.to_string());

    let crate_name = std::env::var("CARGO_PKG_NAME")
        .map_err(|_| {
            syn::Error::new_spanned(
                struct_name,
                "#[derive(Probe)] requires CARGO_PKG_NAME env var (set automatically by cargo)",
            )
        })?
        .replace('-', "_");
    let provider = crate_name.clone();
    let target = format!("{crate_name}_probe");
    let usdt_mod = format_ident!("__kithara_probe_struct_{}", snake);

    let fields = match &input.data {
        Data::Struct(DataStruct {
            fields: Fields::Named(named),
            ..
        }) => &named.named,
        Data::Struct(_) => {
            return Err(syn::Error::new_spanned(
                struct_name,
                "#[derive(Probe)] requires a struct with named fields",
            ));
        }
        _ => {
            return Err(syn::Error::new_spanned(
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
            .ok_or_else(|| syn::Error::new_spanned(field, "expected named field"))?;
        let wire = opts.rename.unwrap_or_else(|| ident.to_string());
        field_idents.push(ident);
        wire_names.push(wire);
    }

    let slot_idents: Vec<Ident> = (0..field_idents.len())
        .map(|i| format_ident!("__probe_slot_{}", i))
        .collect();

    let bindings = field_idents
        .iter()
        .zip(slot_idents.iter())
        .map(|(field, slot)| {
            quote! {
                let #slot: u64 = ::kithara_probes::IntoProbeArg::into_probe_arg(self.#field);
            }
        });

    let usdt_params: Vec<TokenStream2> = (0..slot_idents.len())
        .map(|i| {
            let p = format_ident!("_a{i}");
            quote! { #p: u64 }
        })
        .collect();

    let usdt_call_args = slot_idents.iter();
    let tracing_pairs = wire_names
        .iter()
        .zip(slot_idents.iter())
        .map(|(name, slot)| {
            let ident = format_ident!("{}", name);
            quote! { #ident = #slot }
        });

    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    Ok(quote! {
        impl #impl_generics ::kithara_probes::Probe for #struct_name #ty_generics #where_clause {
            #[inline]
            #[cfg_attr(
                all(feature = "usdt-probes", not(target_arch = "wasm32")),
                expect(
                    unsafe_code,
                    reason = "USDT inline asm in usdt::provider expansion"
                )
            )]
            fn record_probe(&self, name: &'static str) {
                #[cfg(feature = "usdt-probes")]
                {
                    let _ = name;
                    #(#bindings)*

                    #[cfg(not(target_arch = "wasm32"))]
                    {
                        #[::usdt::provider(provider = #provider)]
                        mod #usdt_mod {
                            pub(super) fn record( #(#usdt_params),* ) {}
                        }
                        #usdt_mod::record!(|| ( #(#usdt_call_args),* ));
                    }

                    ::tracing::event!(
                        target: #target,
                        ::tracing::Level::TRACE,
                        probe = name,
                        #(#tracing_pairs),*
                    );
                }
                #[cfg(not(feature = "usdt-probes"))]
                {
                    let _ = name;
                }
            }
        }
    })
}

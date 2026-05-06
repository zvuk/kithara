//! Expansion logic for `#[kithara::probe]` and `#[derive(Probe)]`.
//!
//! Kept out of `lib.rs` because workspace lint policy forbids non-entry
//! items (functions, structs, helpers) in `lib.rs` / `mod.rs` files.

use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{
    Data, DataStruct, DeriveInput, Error, Field, Fields, FnArg, Ident, ItemFn, LitStr, Pat,
    PatIdent, ReturnType, Token, parse::Parser, punctuated::Punctuated,
};

#[derive(Default)]
pub(crate) struct ProbeFilter {
    /// Names of arguments to record (`None` = all eligible args).
    pub args: Option<Vec<Ident>>,
    /// `probe_return` marker: emit a `Probe::record_probe` call on the
    /// function's return value after the body runs. Used when the
    /// payload of interest is computed inside the body (e.g. `outcome`
    /// in a resolver) and surfaced via the return type instead of an
    /// argument. The return type must implement `kithara_probes::Probe`
    /// — typically via `#[derive(kithara_probes::Probe)]`.
    pub probe_return: bool,
}

pub(crate) fn parse_filter(attr: TokenStream2) -> syn::Result<ProbeFilter> {
    if attr.is_empty() {
        return Ok(ProbeFilter::default());
    }
    let parser = Punctuated::<Ident, Token![,]>::parse_terminated;
    let parsed = parser.parse2(attr)?;
    let mut filter = ProbeFilter::default();
    let mut args: Vec<Ident> = Vec::new();
    for ident in parsed {
        if ident == "probe_return" {
            filter.probe_return = true;
        } else {
            args.push(ident);
        }
    }
    if !args.is_empty() {
        filter.args = Some(args);
    }
    Ok(filter)
}

pub(crate) fn expand(input: &ItemFn, filter: ProbeFilter) -> syn::Result<TokenStream2> {
    let fn_name = input.sig.ident.clone();
    let fn_name_str = fn_name.to_string();

    let crate_name = std::env::var("CARGO_PKG_NAME")
        .map_err(|_| {
            Error::new_spanned(
                &input.sig.ident,
                "#[kithara::probe] requires CARGO_PKG_NAME env var (set automatically by cargo)",
            )
        })?
        .replace('-', "_");
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
                    return Err(Error::new_spanned(
                        other,
                        "#[kithara::probe] requires plain named arguments (no patterns)",
                    ));
                }
            },
        }
    }

    let arg_idents: Vec<Ident> = match filter.args {
        None => all_args,
        Some(names) => {
            if let Some(missing) = names
                .iter()
                .find(|name| !all_args.iter().any(|a| a == *name))
            {
                return Err(Error::new_spanned(
                    missing,
                    format!(
                        "#[kithara::probe(...)] arg `{missing}` does not match any function parameter"
                    ),
                ));
            }
            names
        }
    };
    if arg_idents.len() > 6 {
        return Err(Error::new_spanned(
            &input.sig.ident,
            "#[kithara::probe] supports at most 6 wire arguments \
             (USDT provider arity ceiling). Pass fewer fields, fold them \
             into a single struct via `#[derive(Probe)]`, or split the \
             function so each probe site stays under the limit.",
        ));
    }
    let probe_return = filter.probe_return;

    let probe_idents: Vec<Ident> = (0..arg_idents.len())
        .map(|i| format_ident!("__probe_arg_{}", i))
        .collect();

    let arg_bindings: Vec<TokenStream2> = arg_idents
        .iter()
        .zip(probe_idents.iter())
        .map(|(arg, slot)| {
            quote! {
                #[cfg(any(feature = "tracing-probes", feature = "usdt-probes"))]
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

    let fire_fn = format_ident!("fire_{}", arg_idents.len());

    let tracing_fields: Vec<TokenStream2> = arg_idents
        .iter()
        .zip(probe_idents.iter())
        .map(|(name, slot)| quote! { #name = #slot })
        .collect();

    let attrs = &input.attrs;
    let vis = &input.vis;
    let sig = &input.sig;
    let block = &input.block;

    let body = if probe_return {
        // Wrap the body in a closure so any `return X;` inside `#block`
        // becomes a `return X;` from the closure — `__probe_ret` then
        // captures the actual final value regardless of which branch
        // returned it. The return type is inferred from the closure's
        // return expressions; matching `sig.output` annotation on the
        // closure would force callers to repeat the type and confuses
        // type inference for `Option<...>` / generic returns.
        let _ = ReturnType::Default; // keep ReturnType import live
        quote! {
            let __probe_ret = (|| #block)();
            #[cfg(any(feature = "tracing-probes", feature = "usdt-probes"))]
            ::kithara_probes::Probe::record_probe(&__probe_ret, #fn_name_str);
            __probe_ret
        }
    } else {
        quote! { #block }
    };

    // Skip the entry-time `tracing::event!` emission when the macro
    // is asked to publish via `record_probe` on the return value:
    // emitting both would create two events with the same `probe = "..."`
    // tag — the entry one with no fields, the return one with the
    // actual payload. Consumers that filter `events_with_probe(name)`
    // would get the wrong shape (whichever fired first).
    //
    // The cfg-gate must live *inside* this branch: emitting a bare
    // `#[cfg(...)]` attribute before an empty `quote!{}` would attach
    // the attribute to whatever syntactic item follows in the outer
    // `quote!` (the first statement of `#body`), silently disabling it
    // when the probe features are off.
    let emit_entry_event = if probe_return {
        quote! {}
    } else {
        quote! {
            #[cfg(any(feature = "tracing-probes", feature = "usdt-probes"))]
            ::tracing::event!(
                target: #target,
                ::tracing::Level::TRACE,
                probe = #fn_name_str,
                #(#tracing_fields),*
            );
        }
    };

    Ok(quote! {
        #(#attrs)*
        #vis #sig {
            #(#arg_consume)*
            #(#arg_bindings)*

            // USDT emit goes through the safe `kithara_probes::fire_N`
            // wrappers — all `unsafe` inline asm lives in
            // `kithara-probes::usdt_wire`, so consumer crates can keep
            // `#![forbid(unsafe_code)]` and still publish probes.
            #[cfg(any(feature = "tracing-probes", feature = "usdt-probes"))]
            ::kithara_probes::#fire_fn(#fn_name_str, #(#probe_idents),*);

            #emit_entry_event

            #body
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
    // Reserve a few extra bytes for the underscores we may insert.
    const EXTRA_CAPACITY: usize = 4;
    let mut out = String::with_capacity(s.len() + EXTRA_CAPACITY);
    s.chars().enumerate().for_each(|(i, ch)| {
        if ch.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    });
    out
}

pub(crate) fn expand_derive(input: &DeriveInput) -> syn::Result<TokenStream2> {
    let struct_name = &input.ident;
    let _ = camel_to_snake; // helper retained for backward-compat callers

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

    let bindings = field_idents
        .iter()
        .zip(slot_idents.iter())
        .map(|(field, slot)| {
            quote! {
                let #slot: u64 = ::kithara_probes::IntoProbeArg::into_probe_arg(self.#field);
            }
        });

    let tracing_pairs = wire_names
        .iter()
        .zip(slot_idents.iter())
        .map(|(name, slot)| {
            let ident = format_ident!("{}", name);
            quote! { #ident = #slot }
        });

    // No-feature ack: fields participate in probe wire only under
    // `tracing-probes` / `usdt-probes`. Without those features the
    // struct fields would look unused to `dead_code`. Touch each one
    // through a zero-codegen `let _ = &self.f;` so the warning stays
    // off and the no-feature build matches production layout exactly.
    let no_feature_consume: Vec<TokenStream2> = field_idents
        .iter()
        .map(|f| quote! { let _ = &self.#f; })
        .collect();

    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    Ok(quote! {
        impl #impl_generics ::kithara_probes::Probe for #struct_name #ty_generics #where_clause {
            #[inline]
            fn record_probe(&self, name: &'static str) {
                #[cfg(any(feature = "tracing-probes", feature = "usdt-probes"))]
                {
                    #(#bindings)*
                    // USDT goes through the safe wrappers in
                    // `kithara-probes::usdt_wire`. No inline asm in the
                    // consumer crate, so `#[derive(Probe)]` is usable
                    // under `#![forbid(unsafe_code)]`.
                    ::kithara_probes::#fire_fn(name, #(#slot_idents),*);
                    ::tracing::event!(
                        target: #target,
                        ::tracing::Level::TRACE,
                        probe = name,
                        #(#tracing_pairs),*
                    );
                }
                #[cfg(not(any(feature = "tracing-probes", feature = "usdt-probes")))]
                {
                    let _ = name;
                    #(#no_feature_consume)*
                }
            }
        }
    })
}

//! Expansion logic for `#[kithara::probe]` and `#[derive(kithara::Probe)]`.
//!
//! Emits two arms per probe site, both gated by
//! `cfg(any(test, feature = "test-utils"))` of the consumer crate:
//!
//! - `kithara_test_utils::probes::fire_N(...)` — USDT entry point.
//!   Values are converted via `IntoProbeArg::into_probe_arg` to the
//!   `u64` wire format. The actual inline-asm USDT emission only fires
//!   when `kithara-test-utils/usdt-probes` is enabled at build time;
//!   otherwise `fire_N` is a no-op stub.
//! - `tracing::event!` — observable from
//!   `kithara_test_utils::probe_capture` and any other tracing
//!   subscriber. Records the same `u64` slot values for parity with
//!   the USDT path.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{
    Data, DataStruct, DeriveInput, Error, Field, Fields, FnArg, Ident, ItemFn, LitStr, Pat,
    PatIdent, Token, parse::Parser, parse_macro_input, punctuated::Punctuated,
};

/// Entry-point for `#[kithara::probe]` — forwarded from `lib.rs`.
pub(crate) fn expand_attr(attr: TokenStream, item: TokenStream) -> TokenStream {
    let attr2 = TokenStream2::from(attr);
    let filter = match parse_filter(attr2) {
        Ok(f) => f,
        Err(e) => return e.into_compile_error().into(),
    };
    let input = parse_macro_input!(item as ItemFn);
    expand(&input, filter)
        .unwrap_or_else(Error::into_compile_error)
        .into()
}

/// Entry-point for `#[derive(kithara::Probe)]` — forwarded from `lib.rs`.
pub(crate) fn expand_derive_entry(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand_derive(&input)
        .unwrap_or_else(Error::into_compile_error)
        .into()
}

/// Parsed `#[kithara::probe(...)]` arguments.
///
/// * `#[kithara::probe]` (no parens) — marker probe: emits only the
///   cheap auto-fields (`seq`, `caller_file`, `caller_line`) and zero
///   wire args. Use this for very-frequent production functions
///   whose parameters are not `IntoProbeArg` (e.g.
///   `Future::poll_next(&self, cx: &mut Context)`).
/// * `#[kithara::probe(field1, field2, …)]` — explicit list of
///   parameter idents to record as wire args (max 6, USDT arity
///   ceiling). Each ident must match a real parameter name.
/// * `#[kithara::probe(caller, …)]` — additionally capture
///   `caller_fn` via `backtrace::trace`. Opt-in because backtrace
///   resolution is ~ms per firing and blows up hot loops; do NOT
///   use on `poll_next`-style hot probes.
/// * `#[kithara::probe(probe_return)]` — record the function's
///   return value through `Probe::record_probe`.
#[derive(Default)]
pub(crate) struct ProbeFilter {
    pub args: Option<Vec<Ident>>,
    pub probe_return: bool,
    pub caller: bool,
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
        } else if ident == "caller" {
            filter.caller = true;
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
            FnArg::Receiver(_) => {}
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

    // `#[kithara::probe]` (no args, no `probe_return`) → marker
    // probe with zero wire fields (only auto-injected `seq` /
    // `caller_fn`). Explicit list `#[kithara::probe(a, b)]` → those
    // parameters as wire fields. There is no implicit "all params"
    // mode: passing zero args means zero args.
    let arg_idents: Vec<Ident> = match filter.args {
        None => {
            // Consume `all_args` so unused-binding warnings don't
            // surface on functions whose params are intentionally
            // outside the probe.
            let _ = all_args;
            Vec::new()
        }
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
                #[cfg(any(test, feature = "test-utils"))]
                let #slot: u64 = ::kithara_test_utils::probes::IntoProbeArg::into_probe_arg(#arg);
            }
        })
        .collect();

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
        quote! {
            let __probe_ret = (|| #block)();
            #[cfg(any(test, feature = "test-utils"))]
            {
                ::kithara_test_utils::probes::register_probes();
                ::kithara_test_utils::probes::Probe::record_probe(&__probe_ret, #fn_name_str);
            }
            __probe_ret
        }
    } else {
        quote! { #block }
    };

    let capture_caller_fn = if filter.caller {
        quote! {
            let __probe_caller_fn = ::kithara_test_utils::probes::caller_fn_above(#fn_name_str)
                .unwrap_or_default();
        }
    } else {
        quote! {
            // Cheap-path probe: skip backtrace capture.
            let __probe_caller_fn = "";
        }
    };

    let emit_entry_event = if probe_return {
        quote! {}
    } else {
        quote! {
            #[cfg(any(test, feature = "test-utils"))]
            {
                ::kithara_test_utils::probes::register_probes();
                let __probe_caller = ::core::panic::Location::caller();
                let __probe_seq: u64 = ::kithara_test_utils::probes::next_probe_seq();
                #capture_caller_fn
                ::kithara_test_utils::probes::#fire_fn(#fn_name_str, #(#probe_idents),*);
                ::tracing::event!(
                    target: #target,
                    ::tracing::Level::TRACE,
                    probe = #fn_name_str,
                    caller_file = __probe_caller.file(),
                    caller_line = __probe_caller.line() as u64,
                    caller_fn = __probe_caller_fn,
                    seq = __probe_seq,
                    #(#tracing_fields),*
                );
            }
        }
    };

    // `#[track_caller]` is what makes `Location::caller()` resolve to
    // the caller of this probe-attributed function rather than the
    // function's own definition site. Gated on test/test-utils so
    // production builds (probe = no-op) don't pay the cost.
    let track_caller_attr = if probe_return {
        // probe_return uses the derived `Probe::record_probe` path,
        // which doesn't honour caller info today. Skip injection so
        // we don't generate a misleading track_caller attribute on a
        // function that won't read Location::caller().
        quote! {}
    } else {
        quote! { #[cfg_attr(any(test, feature = "test-utils"), track_caller)] }
    };

    Ok(quote! {
        #(#attrs)*
        #track_caller_attr
        #vis #sig {
            #(#arg_consume)*
            #(#arg_bindings)*
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
                let #slot: u64 = ::kithara_test_utils::probes::IntoProbeArg::into_probe_arg(self.#field);
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
        impl #impl_generics ::kithara_test_utils::probes::Probe for #struct_name #ty_generics #where_clause {
            #[inline]
            fn record_probe(&self, name: &'static str) {
                let _ = name;
                #(#field_consume)*
                #[cfg(any(test, feature = "test-utils"))]
                {
                    ::kithara_test_utils::probes::register_probes();
                    #(#bindings)*
                    ::kithara_test_utils::probes::#fire_fn(name, #(#slot_idents),*);
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

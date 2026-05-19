use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{Error, Ident, ItemFn, Pat, PatIdent};

use super::parse::ProbeFilter;

/// Collect every named parameter ident from a function signature.
/// Rejects patterns (`(a, b): _`, ref/mut bindings) — the probe
/// macro needs a bare ident to call `IntoProbeArg::into_probe_arg`
/// against, and pattern-bindings have no single name to use.
fn collect_fn_param_idents(input: &ItemFn) -> syn::Result<Vec<Ident>> {
    let mut idents = Vec::new();
    for arg in &input.sig.inputs {
        match arg {
            syn::FnArg::Receiver(_) => {}
            syn::FnArg::Typed(typed) => match typed.pat.as_ref() {
                Pat::Ident(PatIdent { ident, .. }) => idents.push(ident.clone()),
                other => {
                    return Err(Error::new_spanned(
                        other,
                        "#[kithara::probe] requires plain named arguments (no patterns)",
                    ));
                }
            },
        }
    }
    Ok(idents)
}

/// Resolve the optional `#[kithara::probe(a, b, …)]` filter against
/// the function's actual parameter list. `None` means "marker probe"
/// (no wire args). `Some(names)` must reference real parameters —
/// any ident that doesn't match is a hard error.
fn resolve_arg_idents(
    filter_args: Option<Vec<Ident>>,
    all_args: &[Ident],
) -> syn::Result<Vec<Ident>> {
    let Some(names) = filter_args else {
        return Ok(Vec::new());
    };
    if let Some(missing) = names
        .iter()
        .find(|name| !all_args.iter().any(|a| a == *name))
    {
        return Err(Error::new_spanned(
            missing,
            format!("#[kithara::probe(...)] arg `{missing}` does not match any function parameter"),
        ));
    }
    Ok(names)
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

    let all_args = collect_fn_param_idents(input)?;
    let arg_idents = resolve_arg_idents(filter.args, &all_args)?;
    let computed = filter.computed;

    for (name, _) in &computed {
        if arg_idents.iter().any(|a| a == name) {
            return Err(Error::new_spanned(
                name,
                format!(
                    "#[kithara::probe(...)] computed wire-name `{name}` \
                     collides with a parameter passed in the same probe \
                     attribute; pick a different name"
                ),
            ));
        }
    }

    let total_wire = arg_idents.len() + computed.len();
    if total_wire > 6 {
        return Err(Error::new_spanned(
            &input.sig.ident,
            "#[kithara::probe] supports at most 6 wire arguments \
             (USDT provider arity ceiling) — counting both plain \
             parameters and `name = expr` computed values. Pass fewer \
             fields, fold them into a single struct via \
             `#[derive(Probe)]`, or split the function so each probe \
             site stays under the limit.",
        ));
    }
    let probe_return = filter.probe_return;

    let arg_slot_idents: Vec<Ident> = (0..arg_idents.len())
        .map(|i| format_ident!("__probe_arg_{}", i))
        .collect();
    let computed_slot_idents: Vec<Ident> = (0..computed.len())
        .map(|i| format_ident!("__probe_computed_{}", i))
        .collect();

    let arg_bindings: Vec<TokenStream2> = arg_idents
        .iter()
        .zip(arg_slot_idents.iter())
        .map(|(arg, slot)| {
            quote! {
                #[cfg(any(test, feature = "probe"))]
                let #slot: u64 = ::kithara_test_utils::probe::IntoProbeArg::into_probe_arg(#arg);
            }
        })
        .collect();

    let computed_bindings: Vec<TokenStream2> = computed
        .iter()
        .zip(computed_slot_idents.iter())
        .map(|((_, expr), slot)| {
            quote! {
                #[cfg(any(test, feature = "probe"))]
                let #slot: u64 = ::kithara_test_utils::probe::IntoProbeArg::into_probe_arg(#expr);
            }
        })
        .collect();

    let arg_consume: Vec<TokenStream2> =
        arg_idents.iter().map(|a| quote! { let _ = &#a; }).collect();

    let computed_consume: Vec<TokenStream2> = computed
        .iter()
        .map(|(_, expr)| {
            quote! {
                if false {
                    let _ = #expr;
                }
            }
        })
        .collect();

    let fire_fn = format_ident!("fire_{}", total_wire);

    let mut probe_slot_idents: Vec<Ident> = Vec::with_capacity(total_wire);
    probe_slot_idents.extend(arg_slot_idents.iter().cloned());
    probe_slot_idents.extend(computed_slot_idents.iter().cloned());

    let mut tracing_fields: Vec<TokenStream2> = Vec::with_capacity(total_wire);
    for (name, slot) in arg_idents.iter().zip(arg_slot_idents.iter()) {
        tracing_fields.push(quote! { #name = #slot });
    }
    for ((name, _), slot) in computed.iter().zip(computed_slot_idents.iter()) {
        tracing_fields.push(quote! { #name = #slot });
    }

    let attrs = &input.attrs;
    let vis = &input.vis;
    let sig = &input.sig;
    let block = &input.block;

    let body = if probe_return {
        quote! {
            let __probe_ret = (|| #block)();
            #[cfg(any(test, feature = "probe"))]
            {
                ::kithara_test_utils::probe::register_probes();
                ::kithara_test_utils::probe::Probe::record_probe(&__probe_ret, #fn_name_str);
            }
            __probe_ret
        }
    } else {
        quote! { #block }
    };

    let capture_caller_fn = if filter.caller {
        quote! {
            let __probe_caller_fn = ::kithara_test_utils::probe::caller_fn_above(#fn_name_str)
                .unwrap_or_default();
        }
    } else {
        quote! {
            let __probe_caller_fn = "";
        }
    };

    let emit_entry_event = build_emit_entry_event(
        probe_return,
        &fn_name_str,
        &target,
        &fire_fn,
        &probe_slot_idents,
        &tracing_fields,
        &capture_caller_fn,
    );

    let track_caller_attr = if probe_return {
        quote! {}
    } else {
        quote! { #[cfg_attr(any(test, feature = "probe"), track_caller)] }
    };

    Ok(quote! {
        #(#attrs)*
        #track_caller_attr
        #vis #sig {
            #(#arg_consume)*
            #(#computed_consume)*
            #(#arg_bindings)*
            #(#computed_bindings)*
            #emit_entry_event
            #body
        }
    })
}

fn build_emit_entry_event(
    probe_return: bool,
    fn_name_str: &str,
    target: &str,
    fire_fn: &Ident,
    probe_idents: &[Ident],
    tracing_fields: &[TokenStream2],
    capture_caller_fn: &TokenStream2,
) -> TokenStream2 {
    if probe_return {
        return quote! {};
    }
    quote! {
        #[cfg(any(test, feature = "probe"))]
        {
            ::kithara_test_utils::probe::register_probes();
            let __probe_caller = ::core::panic::Location::caller();
            let __probe_seq: u64 = ::kithara_test_utils::probe::next_probe_seq();
            let __probe_thread_seq: u64 =
                ::kithara_test_utils::probe::next_thread_probe_seq();
            let __probe_thread_id: u64 =
                ::kithara_test_utils::probe::current_thread_u64();
            let __probe_install_id: u64 =
                ::kithara_test_utils::probe::current_install_id();
            #capture_caller_fn
            ::kithara_test_utils::probe::#fire_fn(#fn_name_str, #(#probe_idents),*);
            ::tracing::event!(
                target: #target,
                ::tracing::Level::TRACE,
                probe = #fn_name_str,
                caller_file = __probe_caller.file(),
                caller_line = __probe_caller.line() as u64,
                caller_fn = __probe_caller_fn,
                seq = __probe_seq,
                thread_id = __probe_thread_id,
                thread_seq = __probe_thread_seq,
                install_id = __probe_install_id,
                #(#tracing_fields),*
            );
        }
    }
}

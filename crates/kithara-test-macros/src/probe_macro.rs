use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{
    Data, DataStruct, DeriveInput, Error, Expr, Field, Fields, FnArg, Ident, ItemFn, LitStr, Pat,
    PatIdent, Token,
    parse::{Parse, ParseStream, Parser},
    parse_macro_input,
    punctuated::Punctuated,
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

/// Entry-point for `#[derive(kithara::IntoProbeArg)]` — forwarded from `lib.rs`.
pub(crate) fn expand_derive_into_probe_arg_entry(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand_derive_into_probe_arg(&input)
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
/// * `#[kithara::probe(name = expr, …)]` — record a computed value
///   under the wire-name `name`. `expr` is evaluated inside the
///   function body at probe-firing time (so it can read `self`,
///   parameters, locals); its result must implement `IntoProbeArg`.
///   Plain parameter idents and `name = expr` entries may be mixed
///   freely; the combined count counts against the 6-arg ceiling.
/// * `#[kithara::probe(caller, …)]` — additionally capture
///   `caller_fn` via `backtrace::trace`. Opt-in because backtrace
///   resolution is ~ms per firing and blows up hot loops; do NOT
///   use on `poll_next`-style hot probes.
/// * `#[kithara::probe(probe_return)]` — record the function's
///   return value through `Probe::record_probe`.
#[derive(Default, Debug)]
pub(crate) struct ProbeFilter {
    pub args: Option<Vec<Ident>>,
    pub computed: Vec<(Ident, Expr)>,
    pub probe_return: bool,
    pub caller: bool,
}

/// One entry inside `#[kithara::probe(...)]`. Either a bare ident
/// (parameter name or keyword flag) or `name = expr` (computed value).
///
/// `expr` is boxed: `syn::Expr` is large (~256 bytes) and would
/// otherwise dominate the enum size for every entry.
enum ProbeArg {
    Plain(Ident),
    Computed { name: Ident, expr: Box<Expr> },
}

impl Parse for ProbeArg {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let name: Ident = input.parse()?;
        if input.peek(Token![=]) {
            let _eq: Token![=] = input.parse()?;
            let expr: Expr = input.parse()?;
            Ok(Self::Computed {
                name,
                expr: Box::new(expr),
            })
        } else {
            Ok(Self::Plain(name))
        }
    }
}

pub(crate) fn parse_filter(attr: TokenStream2) -> syn::Result<ProbeFilter> {
    if attr.is_empty() {
        return Ok(ProbeFilter::default());
    }
    let parser = Punctuated::<ProbeArg, Token![,]>::parse_terminated;
    let parsed = parser.parse2(attr)?;
    let mut filter = ProbeFilter::default();
    let mut args: Vec<Ident> = Vec::new();
    for entry in parsed {
        match entry {
            ProbeArg::Plain(ident) => {
                if ident == "probe_return" {
                    filter.probe_return = true;
                } else if ident == "caller" {
                    filter.caller = true;
                } else {
                    args.push(ident);
                }
            }
            ProbeArg::Computed { name, expr } => {
                if name == "probe_return" || name == "caller" {
                    return Err(Error::new_spanned(
                        &name,
                        format!(
                            "#[kithara::probe(...)] `{name}` is a reserved keyword \
                             and cannot be used as the name of a `name = expr` entry"
                        ),
                    ));
                }
                filter.computed.push((name, *expr));
            }
        }
    }
    if !args.is_empty() {
        filter.args = Some(args);
    }
    Ok(filter)
}

/// Collect every named parameter ident from a function signature.
/// Rejects patterns (`(a, b): _`, ref/mut bindings) — the probe
/// macro needs a bare ident to call `IntoProbeArg::into_probe_arg`
/// against, and pattern-bindings have no single name to use.
fn collect_fn_param_idents(input: &ItemFn) -> syn::Result<Vec<Ident>> {
    let mut idents = Vec::new();
    for arg in &input.sig.inputs {
        match arg {
            FnArg::Receiver(_) => {}
            FnArg::Typed(typed) => match typed.pat.as_ref() {
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
    // `#[kithara::probe]` (no args, no `probe_return`) → marker
    // probe with zero wire fields (only auto-injected `seq` /
    // `caller_fn`). Explicit list `#[kithara::probe(a, b)]` → those
    // parameters as wire fields. There is no implicit "all params"
    // mode: passing zero args means zero args.
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

    // Computed wire-names must not clash with parameter wire-names
    // (would produce two slots labelled the same on tracing/USDT side).
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
                #[cfg(any(test, feature = "test-utils"))]
                let #slot: u64 = ::kithara_test_utils::probes::IntoProbeArg::into_probe_arg(#arg);
            }
        })
        .collect();

    let computed_bindings: Vec<TokenStream2> = computed
        .iter()
        .zip(computed_slot_idents.iter())
        .map(|((_, expr), slot)| {
            quote! {
                #[cfg(any(test, feature = "test-utils"))]
                let #slot: u64 = ::kithara_test_utils::probes::IntoProbeArg::into_probe_arg(#expr);
            }
        })
        .collect();

    let arg_consume: Vec<TokenStream2> =
        arg_idents.iter().map(|a| quote! { let _ = &#a; }).collect();

    // Reference computed expressions unconditionally so production
    // builds (no `test-utils` feature → probe expansion compiled out)
    // still see the underlying fields/methods used and avoid spurious
    // `dead_code` warnings on probe-only payload fields. Gated behind
    // `if false` so LLVM constant-folds the call out — zero runtime cost.
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

    let emit_entry_event = build_emit_entry_event(
        probe_return,
        &fn_name_str,
        &target,
        &fire_fn,
        &probe_slot_idents,
        &tracing_fields,
        &capture_caller_fn,
    );

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
        #[cfg(any(test, feature = "test-utils"))]
        {
            ::kithara_test_utils::probes::register_probes();
            let __probe_caller = ::core::panic::Location::caller();
            let __probe_seq: u64 = ::kithara_test_utils::probes::next_probe_seq();
            let __probe_thread_seq: u64 =
                ::kithara_test_utils::probes::next_thread_probe_seq();
            let __probe_thread_id: u64 =
                ::kithara_test_utils::probes::current_thread_u64();
            let __probe_install_id: u64 =
                ::kithara_test_utils::probes::current_install_id();
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
                thread_id = __probe_thread_id,
                thread_seq = __probe_thread_seq,
                install_id = __probe_install_id,
                #(#tracing_fields),*
            );
        }
    }
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
                quote!(Self(<#ty as ::kithara_test_utils::probes::IntoProbeArg>::from_probe_arg(packed))),
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
                    #name: <#ty as ::kithara_test_utils::probes::IntoProbeArg>::from_probe_arg(packed),
                }),
            )
        }
    };

    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    Ok(quote! {
        impl #impl_generics ::kithara_test_utils::probes::IntoProbeArg
        for #struct_name #ty_generics #where_clause {
            fn into_probe_arg(self) -> u64 {
                <#field_ty as ::kithara_test_utils::probes::IntoProbeArg>::into_probe_arg(#field_access)
            }
            fn from_probe_arg(packed: u64) -> Self {
                #ctor
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use quote::quote;

    use super::parse_filter;

    #[test]
    fn empty_attr_yields_default_filter() {
        let f = parse_filter(quote!()).unwrap();
        assert!(f.args.is_none());
        assert!(f.computed.is_empty());
        assert!(!f.probe_return);
        assert!(!f.caller);
    }

    #[test]
    fn plain_idents_only() {
        let f = parse_filter(quote!(variant, budget)).unwrap();
        let args = f.args.expect("plain idents present");
        assert_eq!(args.len(), 2);
        assert_eq!(args[0].to_string(), "variant");
        assert_eq!(args[1].to_string(), "budget");
        assert!(f.computed.is_empty());
    }

    #[test]
    fn keyword_flags_recognised() {
        let f = parse_filter(quote!(probe_return)).unwrap();
        assert!(f.probe_return);
        assert!(!f.caller);
        assert!(f.args.is_none());

        let f = parse_filter(quote!(caller, variant)).unwrap();
        assert!(f.caller);
        assert_eq!(f.args.as_ref().unwrap().len(), 1);
    }

    #[test]
    fn computed_name_eq_expr_recognised() {
        let f = parse_filter(quote!(variant, queue_len = self.queue.len())).unwrap();
        assert_eq!(f.args.as_ref().unwrap().len(), 1);
        assert_eq!(f.computed.len(), 1);
        assert_eq!(f.computed[0].0.to_string(), "queue_len");
    }

    #[test]
    fn multiple_computed_mixed_with_plain() {
        let f = parse_filter(quote!(
            variant,
            queue_len = self.queue.len(),
            init_state = self.init.state as u32
        ))
        .unwrap();
        let args = f.args.expect("plain idents present");
        assert_eq!(args.len(), 1);
        assert_eq!(f.computed.len(), 2);
        assert_eq!(f.computed[0].0.to_string(), "queue_len");
        assert_eq!(f.computed[1].0.to_string(), "init_state");
    }

    #[test]
    fn computed_keyword_name_is_rejected() {
        let err = parse_filter(quote!(probe_return = self.foo)).expect_err("reserved keyword");
        let msg = err.to_string();
        assert!(msg.contains("probe_return"));
        assert!(msg.contains("reserved keyword"));
    }

    #[test]
    fn caller_as_computed_name_is_rejected() {
        let err = parse_filter(quote!(caller = self.foo)).expect_err("reserved keyword");
        assert!(err.to_string().contains("caller"));
    }
}

//! Unified `#[kithara::test]` proc-macro for native + WASM targets.
//!
//! Replaces `#[test]`, `#[tokio::test]`, `#[multiplatform_test]`, `#[rstest]`,
//! and `#[timeout]` with a single attribute.
//!
//! Also provides `#[kithara::fixture]` as a no-op marker (replaces `#[rstest::fixture]`).

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{
    Attribute, Expr, FnArg, Ident, ItemFn, Pat, Token,
    parse::{Parse, ParseStream, Parser},
    parse_macro_input,
    punctuated::Punctuated,
};

// ---------------------------------------------------------------------------
// Argument parsing
// ---------------------------------------------------------------------------

struct TestArgs {
    is_tokio: bool,
    is_wasm_only: bool,
    timeout: Option<Expr>,
}

impl Parse for TestArgs {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let mut args = TestArgs {
            is_tokio: false,
            is_wasm_only: false,
            timeout: None,
        };

        while !input.is_empty() {
            let ident: Ident = input.parse()?;
            match ident.to_string().as_str() {
                "tokio" => args.is_tokio = true,
                "wasm" => args.is_wasm_only = true,
                "timeout" => {
                    let content;
                    syn::parenthesized!(content in input);
                    args.timeout = Some(content.parse()?);
                }
                other => {
                    return Err(syn::Error::new(
                        ident.span(),
                        format!("unknown argument: {other}"),
                    ));
                }
            }
            if !input.is_empty() {
                input.parse::<Token![,]>()?;
            }
        }

        if args.is_tokio && args.is_wasm_only {
            return Err(syn::Error::new(
                proc_macro2::Span::call_site(),
                "`tokio` and `wasm` are mutually exclusive",
            ));
        }

        Ok(args)
    }
}

// ---------------------------------------------------------------------------
// Case / parameter extraction
// ---------------------------------------------------------------------------

struct Case {
    name: Option<Ident>,
    values: Vec<Expr>,
}

fn is_case_attr(attr: &Attribute) -> bool {
    attr.path()
        .segments
        .first()
        .is_some_and(|s| s.ident == "case")
}

fn extract_cases(attrs: &[Attribute]) -> syn::Result<Vec<Case>> {
    let mut cases = Vec::new();
    for attr in attrs.iter().filter(|a| is_case_attr(a)) {
        let path = attr.path();
        let name = if path.segments.len() > 1 {
            path.segments.last().map(|s| s.ident.clone())
        } else {
            None
        };
        let values = if let syn::Meta::List(list) = &attr.meta {
            Punctuated::<Expr, Token![,]>::parse_terminated
                .parse2(list.tokens.clone())?
                .into_iter()
                .collect()
        } else {
            vec![]
        };
        cases.push(Case { name, values });
    }
    Ok(cases)
}

enum ParamKind {
    Case,
    Future,
    Fixture,
}

struct ParamInfo {
    name: Ident,
    ty: Box<syn::Type>,
    kind: ParamKind,
    mutability: Option<Token![mut]>,
}

fn has_attr(attrs: &[Attribute], name: &str) -> bool {
    attrs
        .iter()
        .any(|a| a.path().segments.first().is_some_and(|s| s.ident == name))
}

fn extract_params(func: &ItemFn) -> Vec<ParamInfo> {
    func.sig
        .inputs
        .iter()
        .filter_map(|arg| {
            let FnArg::Typed(pt) = arg else { return None };
            let Pat::Ident(pi) = &*pt.pat else {
                return None;
            };
            let kind = if has_attr(&pt.attrs, "case") {
                ParamKind::Case
            } else if has_attr(&pt.attrs, "future") {
                ParamKind::Future
            } else {
                ParamKind::Fixture
            };
            Some(ParamInfo {
                name: pi.ident.clone(),
                ty: pt.ty.clone(),
                kind,
                mutability: pi.mutability,
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Code generation helpers
// ---------------------------------------------------------------------------

fn make_preamble(params: &[ParamInfo], case_values: Option<&[Expr]>) -> TokenStream2 {
    let mut stmts = Vec::new();
    let mut case_idx = 0;

    for p in params {
        let name = &p.name;
        let ty = &p.ty;
        // Strip leading `_` from fixture name to find the actual function.
        // e.g. `_tracing_setup: ()` calls `tracing_setup()`.
        let fn_name = {
            let s = name.to_string();
            let trimmed = s.trim_start_matches('_');
            if trimmed.is_empty() {
                name.clone()
            } else {
                format_ident!("{}", trimmed)
            }
        };
        let mutability = &p.mutability;
        match p.kind {
            ParamKind::Case => {
                if let Some(vals) = case_values
                    && let Some(val) = vals.get(case_idx)
                {
                    stmts.push(quote! { let #mutability #name: #ty = #val; });
                    case_idx += 1;
                }
            }
            ParamKind::Future => {
                // Async fixture: call function, don't await (user does `.await` in body)
                stmts.push(quote! { let #mutability #name = #fn_name(); });
            }
            ParamKind::Fixture => {
                stmts.push(quote! { let #mutability #name: #ty = #fn_name(); });
            }
        }
    }

    quote! { #(#stmts)* }
}

fn make_test_attrs(args: &TestArgs, is_async: bool) -> TokenStream2 {
    let native = if is_async || args.is_tokio {
        quote! { #[cfg_attr(not(target_arch = "wasm32"), tokio::test)] }
    } else {
        quote! { #[cfg_attr(not(target_arch = "wasm32"), test)] }
    };
    let wasm = quote! { #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)] };
    quote! { #native #wasm }
}

fn wrap_with_timeout(body: &TokenStream2, timeout: &Option<Expr>, is_async: bool) -> TokenStream2 {
    let Some(dur) = timeout else {
        return quote! { { #body } };
    };

    if is_async {
        quote! {
            {
                let __body = async { #body };
                #[cfg(not(target_arch = "wasm32"))]
                {
                    tokio::time::timeout(#dur, __body)
                        .await
                        .expect("test timed out")
                }
                #[cfg(target_arch = "wasm32")]
                { __body.await }
            }
        }
    } else {
        quote! {
            {
                let __body = move || { #body };
                #[cfg(not(target_arch = "wasm32"))]
                {
                    let (tx, rx) = std::sync::mpsc::channel();
                    let handle = std::thread::spawn(move || { tx.send(__body()).ok(); });
                    rx.recv_timeout(#dur).expect("test timed out");
                    handle.join().ok();
                }
                #[cfg(target_arch = "wasm32")]
                { __body() }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_one_test(
    fn_name: &Ident,
    vis: &syn::Visibility,
    ret_type: &syn::ReturnType,
    remaining_attrs: &[&Attribute],
    test_attrs: &TokenStream2,
    is_async: bool,
    preamble: &TokenStream2,
    body_stmts: &[syn::Stmt],
    args: &TestArgs,
) -> TokenStream2 {
    let full = quote! { #preamble #(#body_stmts)* };
    let wrapped = wrap_with_timeout(&full, &args.timeout, is_async);
    let asyncness = is_async.then(|| quote! { async });

    quote! {
        #(#remaining_attrs)*
        #test_attrs
        #vis #asyncness fn #fn_name() #ret_type #wrapped
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Unified test attribute.
///
/// ```text
/// #[kithara::test]                                     // sync
/// #[kithara::test(tokio)]                              // async
/// #[kithara::test(wasm)]                               // wasm-only
/// #[kithara::test(timeout(Duration::from_secs(5)))]    // sync + timeout
/// #[kithara::test(tokio, timeout(Duration::from_secs(5)))]  // async + timeout
/// ```
///
/// Supports `#[case]` / `#[case::name]` parameterization and fixture injection.
#[proc_macro_attribute]
pub fn test(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr as TestArgs);
    let func = parse_macro_input!(item as ItemFn);

    match generate(args, func) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

fn generate(args: TestArgs, func: ItemFn) -> syn::Result<TokenStream2> {
    let cases = extract_cases(&func.attrs)?;
    let remaining_attrs: Vec<_> = func.attrs.iter().filter(|a| !is_case_attr(a)).collect();
    let params = extract_params(&func);
    let is_async = func.sig.asyncness.is_some();
    let vis = &func.vis;
    let fn_name = &func.sig.ident;
    let ret_type = &func.sig.output;
    let body_stmts = &func.block.stmts;

    // wasm-only: cfg(wasm32) + wasm_bindgen_test, no native counterpart
    if args.is_wasm_only {
        if cases.is_empty() {
            let preamble = make_preamble(&params, None);
            return Ok(quote! {
                #(#remaining_attrs)*
                #[cfg(target_arch = "wasm32")]
                #[wasm_bindgen_test::wasm_bindgen_test]
                #vis async fn #fn_name() #ret_type {
                    #preamble
                    #(#body_stmts)*
                }
            });
        }
        let mut tests = TokenStream2::new();
        for (i, case) in cases.iter().enumerate() {
            let case_name = match &case.name {
                Some(name) => format_ident!("{}_{}", fn_name, name),
                None => format_ident!("{}_case_{}", fn_name, i + 1),
            };
            let preamble = make_preamble(&params, Some(&case.values));
            tests.extend(quote! {
                #(#remaining_attrs)*
                #[cfg(target_arch = "wasm32")]
                #[wasm_bindgen_test::wasm_bindgen_test]
                #vis async fn #case_name() #ret_type {
                    #preamble
                    #(#body_stmts)*
                }
            });
        }
        return Ok(tests);
    }

    let test_attrs = make_test_attrs(&args, is_async);

    if cases.is_empty() {
        // Single test — inject fixtures only
        let preamble = make_preamble(&params, None);
        Ok(emit_one_test(
            fn_name,
            vis,
            ret_type,
            &remaining_attrs,
            &test_attrs,
            is_async,
            &preamble,
            body_stmts,
            &args,
        ))
    } else {
        // One test per case
        let mut tests = TokenStream2::new();
        for (i, case) in cases.iter().enumerate() {
            let case_name = match &case.name {
                Some(name) => format_ident!("{}_{}", fn_name, name),
                None => format_ident!("{}_case_{}", fn_name, i + 1),
            };
            let preamble = make_preamble(&params, Some(&case.values));
            tests.extend(emit_one_test(
                &case_name,
                vis,
                ret_type,
                &remaining_attrs,
                &test_attrs,
                is_async,
                &preamble,
                body_stmts,
                &args,
            ));
        }
        Ok(tests)
    }
}

/// Fixture marker — resolves dependencies from function parameters.
///
/// A zero-arg fixture passes through unchanged:
/// ```text
/// #[kithara::fixture]
/// fn my_fixture() -> MyType { ... }
/// ```
///
/// A fixture with parameters is transformed to call each dependency:
/// ```text
/// #[kithara::fixture]
/// fn disk_store(temp_dir: TempDir) -> DiskStore { ... }
/// // ↓ expands to ↓
/// fn disk_store() -> DiskStore {
///     let temp_dir: TempDir = temp_dir();
///     // original body
/// }
/// ```
#[proc_macro_attribute]
pub fn fixture(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let func = parse_macro_input!(item as ItemFn);

    // No params → pass through unchanged
    if func.sig.inputs.is_empty() {
        return quote! { #func }.into();
    }

    // Has params → transform to zero-arg, resolving deps internally
    let vis = &func.vis;
    let fn_name = &func.sig.ident;
    let ret_type = &func.sig.output;
    let body_stmts = &func.block.stmts;
    let asyncness = func.sig.asyncness;
    let remaining_attrs: Vec<_> = func.attrs.iter().collect();

    let dep_bindings: Vec<_> = func
        .sig
        .inputs
        .iter()
        .filter_map(|arg| {
            let FnArg::Typed(pt) = arg else { return None };
            let Pat::Ident(pi) = &*pt.pat else {
                return None;
            };
            let name = &pi.ident;
            let ty = &pt.ty;
            let mutability = &pi.mutability;
            // Strip leading `_` to find the actual fixture function name.
            let dep_fn = {
                let s = name.to_string();
                let trimmed = s.trim_start_matches('_');
                if trimmed.is_empty() {
                    name.clone()
                } else {
                    format_ident!("{}", trimmed)
                }
            };
            Some(quote! { let #mutability #name: #ty = #dep_fn(); })
        })
        .collect();

    quote! {
        #(#remaining_attrs)*
        #vis #asyncness fn #fn_name() #ret_type {
            #(#dep_bindings)*
            #(#body_stmts)*
        }
    }
    .into()
}

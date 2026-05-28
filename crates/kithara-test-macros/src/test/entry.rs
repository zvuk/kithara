#![allow(
    clippy::needless_pass_by_value,
    reason = "proc_macro parse_macro_input! produces owned values"
)]

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{Attribute, Expr, Ident, ItemFn, parse_macro_input};

use super::{
    case::{Case, case_ident, extract_cases, is_case_attr},
    expand_async::emit_browser_test,
    expand_sync::{emit_native_only_one, emit_one_test},
    fixture_args::{ParamInfo, extract_params, make_preamble},
    parse::TestArgs,
    shared::{
        finalize_body, make_dedicated_worker_config, make_serial_attr, make_sync_test_attrs,
        make_tracing_init, wrap_with_timeout,
    },
};

/// Unified test attribute.
///
/// ```text
/// #[kithara::test]                                     // sync, native + wasm
/// #[kithara::test(tokio)]                              // async, native + wasm
/// #[kithara::test(wasm)]                               // wasm-only (async)
/// #[kithara::test(native)]                             // native-only
/// #[kithara::test(native, tokio)]                      // native-only async
/// #[kithara::test(browser)]                            // wasm-only, browser with thread pool init
/// #[kithara::test(native, browser)]                    // sync native + browser wasm
/// #[kithara::test(tokio, browser)]                     // async native + browser wasm
/// #[kithara::test(timeout(Duration::from_secs(5)))]    // sync + timeout
/// #[kithara::test(tokio, timeout(Duration::from_secs(5)))]  // async + timeout
/// #[kithara::test(env(NO_PROXY = "host.example.com"))] // set env vars
/// #[kithara::test(tracing("kithara_hls=debug,kithara_stream=debug"))] // custom tracing filter
/// #[kithara::test(soft_fail("connection", "refused"))] // soft-fail on matching panics
/// #[kithara::test(tokio, serial)]                      // serial execution (no parallel)
/// #[kithara::test(tokio, multi_thread)]                 // multi-thread tokio runtime
/// #[kithara::test(selenium)]                            // selenium: native + serial + multi_thread + ignore
/// #[kithara::test(selenium, timeout(Duration::from_secs(120)))]
/// ```
///
/// ## `env`
///
/// `env(KEY = "value")` sets environment variables before the test body runs.
///
/// ## `tracing`
///
/// `tracing("directives")` initializes tracing with a custom `EnvFilter`
/// directive string. When omitted, tests default to `warn`.
///
/// ## `soft_fail`
///
/// `soft_fail("pattern1", "pattern2")` catches panics whose message contains
/// any of the given substrings (case-insensitive). Matching panics are printed
/// as `[SOFT FAIL]` warnings; non-matching panics propagate normally.
///
/// Requires `futures` crate at the call site for async tests.
///
/// ## `serial`
///
/// `serial` emits `#[serial_test::serial]` on each generated test function,
/// preventing parallel execution with other `serial` tests. Use for
/// resource-intensive tests that are sensitive to CPU/IO contention.
///
/// Requires `serial_test` crate at the call site.
///
/// ## `multi_thread`
///
/// Uses `tokio::runtime::Builder::new_multi_thread().worker_threads(2)` instead
/// of `new_current_thread()`.  Required when the test body spawns tasks that
/// need a multi-threaded executor (e.g. `thirtyfour` `WebDriver`).
///
/// ## `selenium`
///
/// Convenience flag that implies `native + tokio + serial + multi_thread` and
/// adds `#[ignore = "requires selenium"]`.  Designed for Selenium/WebDriver
/// integration tests that need a multi-threaded runtime and serial execution.
///
/// ## `browser`
///
/// The `browser` flag injects `kithara_platform::tokio::ensure_thread_pool().await` on
/// WASM to initialize Web Workers before running the test body.
///
/// Supports `#[case]` / `#[case::name]` parameterization and fixture injection.
pub(crate) fn expand(attr: TokenStream, item: TokenStream) -> TokenStream {
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
    let ctx = GenCtx {
        args: &args,
        params: &params,
        remaining_attrs: &remaining_attrs,
        cases: &cases,
        is_async: func.sig.asyncness.is_some(),
        vis: &func.vis,
        fn_name: &func.sig.ident,
        ret_type: &func.sig.output,
        body_stmts: &func.block.stmts,
    };

    if args.is_wasm_only {
        return Ok(generate_wasm_only(&ctx));
    }
    if args.is_native_only && !args.is_browser {
        return Ok(generate_native_only(&ctx));
    }
    if args.is_browser {
        return Ok(generate_browser(&ctx));
    }
    Ok(generate_default(&ctx))
}

/// Shared context for the `generate*` per-branch helpers.
pub(crate) struct GenCtx<'a> {
    pub(crate) fn_name: &'a Ident,
    pub(crate) ret_type: &'a syn::ReturnType,
    pub(crate) args: &'a TestArgs,
    pub(crate) vis: &'a syn::Visibility,
    pub(crate) remaining_attrs: &'a [&'a Attribute],
    pub(crate) cases: &'a [Case],
    pub(crate) params: &'a [ParamInfo],
    pub(crate) body_stmts: &'a [syn::Stmt],
    pub(crate) is_async: bool,
}

/// wasm-only: `cfg(wasm32)` + `wasm_bindgen_test`, no native counterpart.
/// Also emits `run_in_dedicated_worker` config so the test runner uses a
/// browser-based Web Worker instead of Node.js (required by wasm-bindgen-rayon).
fn generate_wasm_only(ctx: &GenCtx<'_>) -> TokenStream2 {
    let worker_config = make_dedicated_worker_config();
    let mut tests = TokenStream2::new();
    tests.extend(worker_config);

    let emit = |name: &Ident, case_values: Option<&[Expr]>| -> TokenStream2 {
        let preamble = make_preamble(ctx.params, case_values);
        let tracing_init = make_tracing_init(ctx.args);
        let body_stmts = ctx.body_stmts;
        let full = quote! { { #tracing_init #preamble #(#body_stmts)* } };
        let with_timeout = wrap_with_timeout(&full, &ctx.args.timeout, true, name);
        let wrapped = finalize_body(&with_timeout, ctx.args, name, true);
        let remaining_attrs = ctx.remaining_attrs;
        let vis = ctx.vis;
        let ret_type = ctx.ret_type;
        quote! {
            #(#remaining_attrs)*
            #[cfg(target_arch = "wasm32")]
            #[wasm_bindgen_test::wasm_bindgen_test]
            #vis async fn #name() #ret_type #wrapped
        }
    };

    if ctx.cases.is_empty() {
        tests.extend(emit(ctx.fn_name, None));
    } else {
        for (i, case) in ctx.cases.iter().enumerate() {
            let case_name = case_ident(ctx.fn_name, case, i);
            tests.extend(emit(&case_name, Some(&case.values)));
        }
    }
    tests
}

/// native-only (without browser): `cfg(not(wasm32))` + `#[test]` or `#[tokio::test]`.
fn generate_native_only(ctx: &GenCtx<'_>) -> TokenStream2 {
    let native_is_async = ctx.is_async || ctx.args.is_tokio;
    let serial_attr = make_serial_attr(ctx.args);
    let mut tests = TokenStream2::new();

    let mut emit_one = |name: &Ident, case_values: Option<&[Expr]>| {
        let preamble = make_preamble(ctx.params, case_values);
        let tracing_init = make_tracing_init(ctx.args);
        let body_stmts = ctx.body_stmts;
        let full = quote! { #tracing_init #preamble #(#body_stmts)* };
        tests.extend(emit_native_only_one(
            ctx,
            name,
            &full,
            &serial_attr,
            native_is_async,
        ));
    };

    if ctx.cases.is_empty() {
        emit_one(ctx.fn_name, None);
    } else {
        for (i, case) in ctx.cases.iter().enumerate() {
            let case_name = case_ident(ctx.fn_name, case, i);
            emit_one(&case_name, Some(&case.values));
        }
    }
    tests
}

/// browser: WASM with `tokio::ensure_thread_pool` init, optionally dual-platform.
///   browser alone      → wasm-only + init
///   native, browser    → sync native + browser wasm
///   tokio, browser     → async native + browser wasm
fn generate_browser(ctx: &GenCtx<'_>) -> TokenStream2 {
    let browser_only = !ctx.args.is_tokio && !ctx.args.is_native_only;
    let mut tests = TokenStream2::new();

    let mut emit = |name: &Ident, case_values: Option<&[Expr]>| {
        let preamble = make_preamble(ctx.params, case_values);
        tests.extend(emit_browser_test(
            name,
            ctx.vis,
            ctx.ret_type,
            ctx.remaining_attrs,
            ctx.is_async,
            &preamble,
            ctx.body_stmts,
            ctx.args,
            browser_only,
        ));
    };

    if ctx.cases.is_empty() {
        emit(ctx.fn_name, None);
    } else {
        for (i, case) in ctx.cases.iter().enumerate() {
            let case_name = case_ident(ctx.fn_name, case, i);
            emit(&case_name, Some(&case.values));
        }
    }
    tests
}

/// Default branch: sync native + WASM, or single-platform when async. One test per case.
fn generate_default(ctx: &GenCtx<'_>) -> TokenStream2 {
    let test_attrs = make_sync_test_attrs();
    let mut tests = TokenStream2::new();

    let mut emit = |name: &Ident, case_values: Option<&[Expr]>| {
        let preamble = make_preamble(ctx.params, case_values);
        tests.extend(emit_one_test(
            name,
            ctx.vis,
            ctx.ret_type,
            ctx.remaining_attrs,
            &test_attrs,
            ctx.is_async,
            &preamble,
            ctx.body_stmts,
            ctx.args,
        ));
    };

    if ctx.cases.is_empty() {
        emit(ctx.fn_name, None);
    } else {
        for (i, case) in ctx.cases.iter().enumerate() {
            let case_name = case_ident(ctx.fn_name, case, i);
            emit(&case_name, Some(&case.values));
        }
    }
    tests
}

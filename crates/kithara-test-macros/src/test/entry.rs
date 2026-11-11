#![allow(
    clippy::needless_pass_by_value,
    reason = "proc_macro parse_macro_input! produces owned values"
)]

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{Attribute, Error, Expr, Ident, ItemFn, ReturnType, parse_macro_input};

use super::{
    case::{Case, case_ident, extract_cases, is_case_attr},
    expand_async::emit_browser_test,
    expand_sync::{emit_native_only_one, emit_one_test},
    fixture_args::{ParamInfo, extract_params, make_preamble},
    parse::TestArgs,
    shared::{
        finalize_body, make_ambient_stmt, make_dedicated_worker_config, make_serial_attr,
        make_sync_test_attrs, make_test_setup, make_wasm_serial_guard,
        wrap_with_sync_participation, wrap_with_timeout,
    },
};

/// Unified async/wasm test attribute.
///
/// Flags (`tokio`, `wasm`, `native`, `browser`, `loom`, `timeout`, `env`,
/// `tracing`, `soft_fail`, `serial`, `multi_thread`, `selenium`) can be
/// combined and support `#[case]` parameterization plus fixture injection.
/// Native `#[by_ref]` fixtures are owned outside the async poll and borrowed by the body.
pub(crate) fn expand(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr as TestArgs);
    let func = parse_macro_input!(item as ItemFn);

    match generate(args, func) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

fn generate(args: TestArgs, func: ItemFn) -> syn::Result<TokenStream2> {
    if args.is_loom && func.sig.asyncness.is_some() {
        return Err(Error::new_spanned(
            func.sig.asyncness,
            "`loom` requires a synchronous deterministic model; extract the concurrent contract \
             from async integration work",
        ));
    }
    if args.is_loom && !matches!(func.sig.output, ReturnType::Default) {
        return Err(Error::new_spanned(
            &func.sig.output,
            "`loom` tests must return `()` so every failing permutation panics inside the model",
        ));
    }

    let cases = extract_cases(&func.attrs)?;
    let remaining_attrs: Vec<_> = func.attrs.iter().filter(|a| !is_case_attr(a)).collect();
    let params = extract_params(&func)?;

    // The emit paths enter dynamic flash after installing ambient. Ordinary
    // platform time calls then use the same mode in the body and its callees.

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
    pub(crate) ret_type: &'a ReturnType,
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
        let scoped = preamble.scoped(false);
        let preamble = &preamble.body;
        let test_setup = make_test_setup(ctx.args, ctx.remaining_attrs);
        // wasm emission: no per-poll `with_ambient`, the body-held scope is
        // the sole ambient writer — KEEP it.
        let ambient = make_ambient_stmt(ctx.args);
        let body_stmts = ctx.body_stmts;
        let full = quote! { { #test_setup #scoped #preamble #ambient #(#body_stmts)* } };
        let with_timeout = wrap_with_timeout(&full, &ctx.args.timeout, true, name);
        let wrapped = finalize_body(&with_timeout, ctx.args, name, true);
        let serial_guard = make_wasm_serial_guard(ctx.args);
        let remaining_attrs = ctx.remaining_attrs;
        let vis = ctx.vis;
        let ret_type = ctx.ret_type;
        quote! {
            #(#remaining_attrs)*
            #[cfg(target_arch = "wasm32")]
            #[wasm_bindgen_test::wasm_bindgen_test]
            #vis async fn #name() #ret_type {
                #serial_guard
                #wrapped
            }
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
        let scoped = preamble.scoped(false);
        let test_setup = make_test_setup(ctx.args, ctx.remaining_attrs);
        let ambient = make_ambient_stmt(ctx.args);
        let body_stmts = ctx.body_stmts;
        // Plain body for the async-native branches (sole ambient holder there
        // is the per-poll `with_ambient`); held body for the sync branch.
        let full_plain = preamble.with_body(body_stmts);
        let preamble = &preamble.body;
        let sync_body = wrap_with_sync_participation(&quote! { #(#body_stmts)* }, ctx.args);
        let full_held = quote! { #test_setup #scoped #preamble #ambient #sync_body };
        tests.extend(emit_native_only_one(
            ctx,
            name,
            &full_plain,
            &full_held,
            &serial_attr,
            native_is_async,
        ));
    };

    if ctx.cases.is_empty() {
        let name = emitted_name(ctx.fn_name, ctx.args);
        emit_one(&name, None);
    } else {
        for (i, case) in ctx.cases.iter().enumerate() {
            let case_name = case_ident(ctx.fn_name, case, i);
            let name = emitted_name(&case_name, ctx.args);
            emit_one(&name, Some(&case.values));
        }
    }
    tests
}

fn emitted_name(name: &Ident, args: &TestArgs) -> Ident {
    if args.is_loom {
        format_ident!("loom_model_{name}", span = name.span())
    } else {
        name.clone()
    }
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

#[cfg(test)]
mod tests {
    use syn::visit_mut::{self, VisitMut};

    use super::{TestArgs, emitted_name, generate};

    #[derive(Default)]
    struct FixtureContext {
        async_depth: usize,
        calls: Vec<bool>,
    }

    impl VisitMut for FixtureContext {
        fn visit_expr_async_mut(&mut self, expression: &mut syn::ExprAsync) {
            self.async_depth += 1;
            visit_mut::visit_expr_async_mut(self, expression);
            self.async_depth -= 1;
        }

        fn visit_expr_call_mut(&mut self, expression: &mut syn::ExprCall) {
            if let syn::Expr::Path(path) = &*expression.func
                && path.path.is_ident("owner")
            {
                self.calls.push(self.async_depth > 0);
            }
            visit_mut::visit_expr_call_mut(self, expression);
        }
    }

    #[test]
    fn borrowed_native_fixture_is_created_outside_async_poll() -> syn::Result<()> {
        let args = syn::parse_str::<TestArgs>("tokio, native")?;
        let function = syn::parse_str("async fn contract(#[by_ref] owner: &Owner) {}")?;
        let mut expanded = syn::parse2::<syn::File>(generate(args, function)?)?;
        let mut context = FixtureContext::default();
        context.visit_file_mut(&mut expanded);

        assert_eq!(context.calls, [false]);
        Ok(())
    }

    #[test]
    fn borrowed_timeout_fixture_is_created_outside_async_poll() -> syn::Result<()> {
        let args = syn::parse_str::<TestArgs>(
            "tokio, native, timeout(std::time::Duration::from_secs(1))",
        )?;
        let function = syn::parse_str("async fn contract(#[by_ref] owner: &Owner) {}")?;
        let mut expanded = syn::parse2::<syn::File>(generate(args, function)?)?;
        let mut context = FixtureContext::default();
        context.visit_file_mut(&mut expanded);

        assert_eq!(context.calls, [false]);
        Ok(())
    }

    #[test]
    fn ordinary_fixture_stays_inside_async_poll() -> syn::Result<()> {
        let args = syn::parse_str::<TestArgs>("tokio, native")?;
        let function = syn::parse_str("async fn contract(owner: Owner) {}")?;
        let mut expanded = syn::parse2::<syn::File>(generate(args, function)?)?;
        let mut context = FixtureContext::default();
        context.visit_file_mut(&mut expanded);

        assert_eq!(context.calls, [true]);
        Ok(())
    }

    #[test]
    fn borrowed_fixture_accepts_raw_parameter_names() -> syn::Result<()> {
        let args = syn::parse_str::<TestArgs>("tokio, native")?;
        let function = syn::parse_str("async fn contract(#[by_ref] r#type: &Owner) {}")?;
        syn::parse2::<syn::File>(generate(args, function)?)?;
        Ok(())
    }

    #[test]
    fn borrowed_fixture_is_local_to_each_emitted_target() -> syn::Result<()> {
        for (attrs, source, expected) in [
            (
                "native",
                "fn contract(#[by_ref] owner: &Owner) {}",
                vec![false],
            ),
            (
                "wasm",
                "async fn contract(#[by_ref] owner: &Owner) {}",
                vec![true],
            ),
            (
                "tokio, browser",
                "async fn contract(#[by_ref] owner: &Owner) {}",
                vec![true, false],
            ),
            (
                "",
                "async fn contract(#[by_ref] owner: &Owner) {}",
                vec![false, true],
            ),
        ] {
            let args = syn::parse_str::<TestArgs>(attrs)?;
            let function = syn::parse_str(source)?;
            let mut expanded = syn::parse2::<syn::File>(generate(args, function)?)?;
            let mut context = FixtureContext::default();
            context.visit_file_mut(&mut expanded);

            assert_eq!(context.calls, expected, "{attrs}");
        }
        Ok(())
    }

    #[test]
    fn borrowed_fixture_rejects_other_parameter_sources() -> syn::Result<()> {
        for source in [
            "async fn contract(#[by_ref] owner: Owner) {}",
            "async fn contract(#[by_ref] owner: &mut Owner) {}",
            "async fn contract(#[case] #[by_ref] owner: &Owner) {}",
            "async fn contract(#[future] #[by_ref] owner: &Owner) {}",
            "async fn contract(#[future(awt)] #[by_ref] owner: &Owner) {}",
        ] {
            let args = syn::parse_str::<TestArgs>("native")?;
            let function = syn::parse_str(source)?;
            assert!(generate(args, function).is_err(), "accepted `{source}`");
        }
        Ok(())
    }

    #[test]
    fn loom_tests_have_a_filterable_name() -> syn::Result<()> {
        let args = syn::parse_str::<TestArgs>("loom")?;
        let name = syn::parse_str("concurrent_contract")?;

        assert_eq!(emitted_name(&name, &args), "loom_model_concurrent_contract");
        Ok(())
    }

    #[test]
    fn loom_rejects_async_and_non_unit_tests() -> syn::Result<()> {
        for source in [
            "async fn contract() {}",
            "fn contract() -> Result<(), String> { Ok(()) }",
        ] {
            let args = syn::parse_str::<TestArgs>("loom")?;
            let function = syn::parse_str(source)?;

            assert!(generate(args, function).is_err(), "accepted `{source}`");
        }
        Ok(())
    }

    #[test]
    fn serial_browser_guard_precedes_body_timeout() -> syn::Result<()> {
        let args = syn::parse_str::<TestArgs>(
            "browser, serial, timeout(std::time::Duration::from_secs(10))",
        )?;
        let function = syn::parse_str("async fn stress() {}")?;

        let expanded = generate(args, function)?.to_string();
        let guard = expanded.find("wasm_serial_guard").expect("serial guard");
        let timeout = expanded.find("time :: timeout").expect("body timeout");

        assert!(guard < timeout, "serial wait consumed the body timeout");
        Ok(())
    }

    #[test]
    fn serial_sync_wasm_test_becomes_async() -> syn::Result<()> {
        let args = syn::parse_str::<TestArgs>("serial")?;
        let function = syn::parse_str("fn contract() {}")?;

        let expanded = generate(args, function)?.to_string();

        assert!(expanded.contains("async fn contract"));
        assert!(expanded.contains("wasm_serial_guard"));
        Ok(())
    }

    #[test]
    fn every_emitted_test_starts_its_load_once() -> syn::Result<()> {
        for (attrs, source) in [
            ("", "fn contract() {}"),
            ("", "async fn contract() {}"),
            ("tokio", "async fn contract() {}"),
            (
                "tokio, timeout(std::time::Duration::from_secs(5))",
                "async fn contract() {}",
            ),
            ("tokio, native", "async fn contract() {}"),
            (
                "tokio, native, timeout(std::time::Duration::from_secs(5))",
                "async fn contract() {}",
            ),
            ("browser", "async fn contract() {}"),
        ] {
            let args = syn::parse_str::<TestArgs>(attrs)?;
            let function = syn::parse_str(source)?;

            let expanded = generate(args, function)?.to_string();
            assert_eq!(
                expanded.matches("LoadGuard").count(),
                expanded.matches("fn contract").count(),
                "`{attrs}` `{source}` must start the load once per emitted test: {expanded}"
            );
        }
        Ok(())
    }

    #[test]
    fn should_panic_test_suppresses_panic_dumps() -> syn::Result<()> {
        for source in [
            "#[should_panic]\nfn contract() {}",
            "#[should_panic(expected = \"boom\")]\nasync fn contract() {}",
            "#[cfg_attr(not(target_arch = \"wasm32\"), should_panic(expected = \"boom\"))]\n\
             fn contract() {}",
        ] {
            let args = syn::parse_str::<TestArgs>("")?;
            let function = syn::parse_str(source)?;

            let expanded = generate(args, function)?.to_string();
            assert!(
                expanded.contains("suppress_expected_panic_dumps"),
                "`{source}` must suppress panic dumps"
            );
        }
        Ok(())
    }

    #[test]
    fn ordinary_test_keeps_panic_dumps_armed() -> syn::Result<()> {
        let args = syn::parse_str::<TestArgs>("")?;
        let function = syn::parse_str("fn contract() {}")?;

        let expanded = generate(args, function)?.to_string();

        assert!(!expanded.contains("suppress_expected_panic_dumps"));
        Ok(())
    }

    #[test]
    fn synchronous_native_entries_participate_after_installing_ambient() -> syn::Result<()> {
        for attrs in [
            "",
            "native",
            "serial",
            "native, browser",
            "native, timeout(std::time::Duration::from_secs(2))",
        ] {
            let args = syn::parse_str::<TestArgs>(attrs)?;
            let function = syn::parse_str("fn contract() -> Result<(), String> { Ok(()) }")?;
            let expanded = generate(args, function)?;
            let file = syn::parse2::<syn::File>(expanded)?;
            let native = file
                .items
                .iter()
                .find_map(|item| match item {
                    syn::Item::Fn(function) if function.sig.asyncness.is_none() => Some(function),
                    _ => None,
                })
                .expect("synchronous native entry");
            let expanded = quote::quote!(#native).to_string();
            assert_eq!(expanded.matches("participate_sync").count(), 1, "{attrs}");
            let ambient = expanded.find("ambient_scope").expect("ambient mode");
            let participation = expanded.find("participate_sync").expect("driver ownership");
            assert!(ambient < participation, "{attrs}");
        }
        Ok(())
    }

    #[test]
    fn async_wasm_loom_and_manual_clock_entries_keep_their_owner() -> syn::Result<()> {
        for (attrs, source) in [
            ("tokio, native", "async fn contract() {}"),
            ("", "async fn contract() {}"),
            ("wasm", "fn contract() {}"),
            ("browser", "async fn contract() {}"),
            ("loom", "fn contract() {}"),
            ("flash(false)", "fn contract() {}"),
            ("native, flash(false)", "fn contract() {}"),
            ("native, browser, flash(false)", "fn contract() {}"),
        ] {
            let args = syn::parse_str::<TestArgs>(attrs)?;
            let function = syn::parse_str(source)?;
            let expanded = generate(args, function)?.to_string();
            assert!(
                !expanded.contains("participate_sync"),
                "{attrs}: {expanded}"
            );
        }
        Ok(())
    }
}

//! Unified `#[kithara::test]` proc-macro for native + WASM targets.
//!
//! Replaces `#[test]`, `#[tokio::test]`, `#[multiplatform_test]`, `#[rstest]`,
//! and `#[timeout]` with a single attribute.
//!
#![expect(
    clippy::needless_pass_by_value, // proc_macro parse_macro_input! produces owned values
    clippy::option_if_let_else      // match is more readable for format_ident! case names
)]
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

#[derive(Default)]
struct TestArgs {
    timeout: Option<Expr>,
    tracing_filter: Option<String>,
    env_vars: Vec<(String, String)>,
    /// Substring patterns for soft-fail: if a panic message contains any of
    /// these (case-insensitive), the test prints a warning instead of failing.
    soft_fail_patterns: Vec<String>,
    is_browser: bool,
    is_multi_thread: bool,
    is_native_only: bool,
    is_selenium: bool,
    is_serial: bool,
    is_tokio: bool,
    is_wasm_only: bool,
}

impl TestArgs {
    fn validate(&mut self) -> syn::Result<()> {
        Self::check_exclusive(self.is_tokio, "tokio", self.is_wasm_only, "wasm")?;
        Self::check_exclusive(self.is_wasm_only, "wasm", self.is_native_only, "native")?;
        Self::check_exclusive(self.is_browser, "browser", self.is_wasm_only, "wasm")?;
        Self::check_exclusive(self.is_selenium, "selenium", self.is_wasm_only, "wasm")?;
        Self::check_exclusive(self.is_selenium, "selenium", self.is_browser, "browser")?;

        // selenium implies native + tokio + serial + multi_thread
        if self.is_selenium {
            self.is_native_only = true;
            self.is_tokio = true;
            self.is_serial = true;
            self.is_multi_thread = true;
        }

        if self.is_multi_thread && !self.is_tokio {
            return Err(syn::Error::new(
                proc_macro2::Span::call_site(),
                "`multi_thread` requires `tokio` (or `selenium` which implies it)",
            ));
        }

        Ok(())
    }

    fn check_exclusive(a: bool, a_name: &str, b: bool, b_name: &str) -> syn::Result<()> {
        if a && b {
            Err(syn::Error::new(
                proc_macro2::Span::call_site(),
                format!("`{a_name}` and `{b_name}` are mutually exclusive"),
            ))
        } else {
            Ok(())
        }
    }
}

fn parse_comma_separated<T>(
    input: ParseStream<'_>,
    mut parse_item: impl FnMut(ParseStream<'_>) -> syn::Result<T>,
) -> syn::Result<Vec<T>> {
    let content;
    syn::parenthesized!(content in input);
    let mut items = Vec::new();
    while !content.is_empty() {
        items.push(parse_item(&content)?);
        if !content.is_empty() {
            content.parse::<Token![,]>()?;
        }
    }
    Ok(items)
}

impl Parse for TestArgs {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let mut args = Self::default();

        while !input.is_empty() {
            let ident: Ident = input.parse()?;
            match ident.to_string().as_str() {
                "tokio" => args.is_tokio = true,
                "wasm" => args.is_wasm_only = true,
                "native" => args.is_native_only = true,
                "browser" => args.is_browser = true,
                "serial" => args.is_serial = true,
                "selenium" => args.is_selenium = true,
                "multi_thread" => args.is_multi_thread = true,
                "timeout" => {
                    let content;
                    syn::parenthesized!(content in input);
                    args.timeout = Some(content.parse()?);
                }
                "env" => {
                    args.env_vars = parse_comma_separated(input, |content| {
                        let key: Ident = content.parse()?;
                        content.parse::<Token![=]>()?;
                        let value: syn::LitStr = content.parse()?;
                        Ok((key.to_string(), value.value()))
                    })?;
                }
                "soft_fail" => {
                    args.soft_fail_patterns = parse_comma_separated(input, |content| {
                        let pattern: syn::LitStr = content.parse()?;
                        Ok(pattern.value())
                    })?;
                    if args.soft_fail_patterns.is_empty() {
                        return Err(syn::Error::new(
                            ident.span(),
                            "soft_fail requires at least one pattern string",
                        ));
                    }
                }
                "tracing" => {
                    let content;
                    syn::parenthesized!(content in input);
                    let value: syn::LitStr = content.parse()?;
                    args.tracing_filter = Some(value.value());
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

        args.validate()?;
        Ok(args)
    }
}

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
    ty: Box<syn::Type>,
    name: Ident,
    mutability: Option<Token![mut]>,
    kind: ParamKind,
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

fn make_preamble(params: &[ParamInfo], case_values: Option<&[Expr]>) -> TokenStream2 {
    let mut stmts = Vec::new();
    let mut case_idx = 0;

    for p in params {
        let name = &p.name;
        let ty = &p.ty;
        // Strip leading `_` from fixture name to find the actual function.
        // e.g. `_temp_dir: TestTempDir` calls `temp_dir()`.
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

fn make_serial_attr(args: &TestArgs) -> TokenStream2 {
    if args.is_serial {
        quote! { #[serial_test::serial] }
    } else {
        quote! {}
    }
}

/// Test attributes for **sync** tests only (dual-platform: native `#[test]` + WASM).
///
/// Async tests are handled separately via [`emit_async_runtime_test`] /
/// [`emit_async_timeout_test`] which create a manual tokio runtime on native.
fn make_sync_test_attrs() -> TokenStream2 {
    let native = quote! { #[cfg_attr(not(target_arch = "wasm32"), test)] };
    let wasm = quote! { #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)] };
    quote! { #native #wasm }
}

/// Generate the tokio runtime builder expression.
///
/// Uses `new_multi_thread().worker_threads(2)` when `multi_thread` or `selenium`
/// is set; otherwise uses `new_current_thread()`.
fn make_runtime_builder(args: &TestArgs) -> TokenStream2 {
    if args.is_multi_thread {
        quote! {
            kithara_platform::tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("kithara test runtime")
        }
    } else {
        quote! {
            kithara_platform::tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("kithara test runtime")
        }
    }
}

fn make_tracing_init(args: &TestArgs) -> TokenStream2 {
    if let Some(filter) = &args.tracing_filter {
        quote! {
            ::kithara_test_utils::setup_tracing_with_filter(#filter);
        }
    } else {
        quote! {
            ::kithara_test_utils::setup_tracing();
        }
    }
}

/// Selenium tests no longer auto-inject `#[ignore]` — the suite runs only
/// when the wasm-target test driver picks them up (`just test-selenium`),
/// so plain `cargo test` already skips them by virtue of platform gating.
fn make_selenium_attrs(_args: &TestArgs) -> TokenStream2 {
    quote! {}
}

/// Emit a native async test with a **manual tokio runtime** (no timeout).
///
/// Generates a sync `#[test]` fn that creates a `current_thread` runtime via
/// `kithara_platform::tokio::runtime::Builder` and calls `block_on`.
///
/// This avoids depending on `#[tokio::test]` so consumer crates do not need
/// `tokio` as a direct dependency.
fn emit_async_runtime_test(
    fn_name: &Ident,
    vis: &syn::Visibility,
    ret_type: &syn::ReturnType,
    remaining_attrs: &[&Attribute],
    body: &TokenStream2,
    args: &TestArgs,
    serial_attr: &TokenStream2,
) -> TokenStream2 {
    let env_setup = make_env_setup(&args.env_vars);
    let selenium_attr = make_selenium_attrs(args);
    let runtime_builder = make_runtime_builder(args);
    let inner_body = if !args.soft_fail_patterns.is_empty() {
        wrap_with_soft_fail(
            &quote! { { #body } },
            fn_name,
            true,
            &args.soft_fail_patterns,
        )
    } else {
        quote! { { #body } }
    };

    quote! {
        #(#remaining_attrs)*
        #serial_attr
        #selenium_attr
        #[cfg(not(target_arch = "wasm32"))]
        #[test]
        #vis fn #fn_name() #ret_type {
            #env_setup
            let __rt = #runtime_builder;
            __rt.block_on(async #inner_body)
        }
    }
}

/// Emit a `const _: ()` block that tells `wasm-bindgen-test-runner` to use a
/// dedicated Web Worker instead of Node.js. Multiple copies are harmless — the
/// anonymous `const _` wrapper prevents name collisions, and the linker merges
/// the `__wasm_bindgen_test_unstable` section entries.
fn make_dedicated_worker_config() -> TokenStream2 {
    quote! {
        #[cfg(target_arch = "wasm32")]
        const _: () = {
            #[unsafe(link_section = "__wasm_bindgen_test_unstable")]
            pub static __WBG_TEST_RUN_IN_DEDICATED_WORKER: [u8; 1] = [0x02u8];
        };
    }
}

fn wrap_with_timeout(
    body: &TokenStream2,
    timeout: &Option<Expr>,
    is_async: bool,
    fn_name: &Ident,
) -> TokenStream2 {
    let Some(dur) = timeout else {
        return quote! { { #body } };
    };

    let fn_name_str = fn_name.to_string();

    if is_async {
        // WASM-only cooperative timeout (native async+timeout uses
        // emit_async_timeout_test for manual runtime control).
        quote! {
            {
                let __timeout_dur: ::std::time::Duration = #dur;
                let __body = async { #body };
                kithara_platform::time::timeout(__timeout_dur, __body)
                    .await
                    .unwrap_or_else(|_| panic!(
                        "test `{}` timed out after {:?}",
                        #fn_name_str, __timeout_dur,
                    ))
            }
        }
    } else {
        quote! {
            {
                let __timeout_dur: ::std::time::Duration = #dur;
                let __body = move || { #body };
                #[cfg(not(target_arch = "wasm32"))]
                {
                    let (tx, rx) = ::std::sync::mpsc::channel();
                    let handle = ::std::thread::spawn(move || {
                        tx.send(__body()).ok();
                    });
                    match rx.recv_timeout(__timeout_dur) {
                        Ok(v) => {
                            handle.join().ok();
                            v
                        }
                        Err(::std::sync::mpsc::RecvTimeoutError::Timeout) => {
                            panic!(
                                "test `{}` timed out after {:?}",
                                #fn_name_str, __timeout_dur,
                            )
                        }
                        Err(::std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                            // Test body panicked — propagate the original panic.
                            match handle.join() {
                                Err(payload) => ::std::panic::resume_unwind(payload),
                                Ok(_) => unreachable!(),
                            }
                        }
                    }
                }
                #[cfg(target_arch = "wasm32")]
                {
                    __body()
                }
            }
        }
    }
}

/// Emit a native async test with a timeout using a **manual tokio runtime**.
///
/// Why: `#[tokio::test]` creates a runtime whose `Drop` waits indefinitely for
/// `spawn_blocking` tasks.  If the test body spawns a blocking thread that
/// outlives the timeout, the runtime shutdown hangs forever — even though
/// `tokio::time::timeout` already fired and panicked.
///
/// This function generates a **sync** `#[test]` fn that:
/// 1. Spawns a watchdog thread **outside** the tokio runtime.
/// 2. Creates the runtime manually.
/// 3. Runs the async body inside `block_on` with `tokio::time::timeout`.
/// 4. Catches panics via `catch_unwind` so we can call `shutdown_timeout`
///    before re-raising.
/// 5. Calls `runtime.shutdown_timeout(100ms)` — never blocks on zombies.
/// 6. The watchdog Drop guard fires **after** `shutdown_timeout`, so it only
///    aborts if even the forced shutdown is stuck.
fn emit_async_timeout_test(
    fn_name: &Ident,
    vis: &syn::Visibility,
    ret_type: &syn::ReturnType,
    remaining_attrs: &[&Attribute],
    body: &TokenStream2,
    args: &TestArgs,
    serial_attr: &TokenStream2,
) -> TokenStream2 {
    let dur = args.timeout.as_ref().expect("caller verified timeout");
    let fn_name_str = fn_name.to_string();
    let timeout_panic_arm = if args.soft_fail_patterns.is_empty() {
        quote! {
            Err(__payload) => ::std::panic::resume_unwind(__payload),
        }
    } else {
        let pattern_strs: Vec<_> = args
            .soft_fail_patterns
            .iter()
            .map(|p| p.to_lowercase())
            .collect();
        quote! {
            Err(__panic) => {
                let __msg = if let Some(s) = __panic.downcast_ref::<&str>() {
                    (*s).to_string()
                } else if let Some(s) = __panic.downcast_ref::<String>() {
                    s.clone()
                } else {
                    "unknown panic".to_string()
                };
                let __lower = __msg.to_lowercase();
                let __patterns: &[&str] = &[#(#pattern_strs),*];
                if __patterns.iter().any(|p| {
                    __lower.contains(p)
                        || (*p == "timeout" && __lower.contains("timed out"))
                }) {
                    eprintln!("[SOFT FAIL] {}: {}", #fn_name_str, __msg);
                } else {
                    ::std::panic::resume_unwind(__panic);
                }
            }
        }
    };

    // Brace the body so it can be used as `async { #braced }` safely.
    let braced = quote! { { #body } };
    let inner_body = if !args.soft_fail_patterns.is_empty() {
        wrap_with_soft_fail(&braced, fn_name, true, &args.soft_fail_patterns)
    } else {
        braced
    };

    let env_setup = make_env_setup(&args.env_vars);
    let selenium_attr = make_selenium_attrs(args);
    let runtime_builder = make_runtime_builder(args);

    quote! {
        #(#remaining_attrs)*
        #serial_attr
        #selenium_attr
        #[cfg(not(target_arch = "wasm32"))]
        #[test]
        #vis fn #fn_name() #ret_type {
            #env_setup

            let __timeout_dur: ::std::time::Duration = #dur;

            // Hard-timeout watchdog — runs OUTSIDE the tokio runtime on a
            // plain OS thread.  A Drop guard cancels it when the function
            // exits (including after runtime.shutdown_timeout).  If even
            // shutdown_timeout hangs, the watchdog fires and aborts.
            let __done = ::std::sync::Arc::new(
                ::std::sync::atomic::AtomicBool::new(false),
            );
            {
                let __done_w = __done.clone();
                let __fn = #fn_name_str;
                ::std::thread::spawn(move || {
                    ::std::thread::sleep(
                        __timeout_dur + ::std::time::Duration::from_secs(3),
                    );
                    if !__done_w.load(::std::sync::atomic::Ordering::SeqCst) {
                        eprintln!(
                            "\n\x1b[1;31mHARD TIMEOUT\x1b[0m: test `{}` exceeded {:?} \
                             (runtime shutdown blocked). Aborting process.\n",
                            __fn, __timeout_dur,
                        );
                        ::std::process::abort();
                    }
                });
            }
            struct __WG(::std::sync::Arc<::std::sync::atomic::AtomicBool>);
            impl Drop for __WG {
                fn drop(&mut self) {
                    self.0.store(true, ::std::sync::atomic::Ordering::SeqCst);
                }
            }
            let _wg = __WG(__done);

            let __rt = #runtime_builder;

            let __result = ::std::panic::catch_unwind(
                ::std::panic::AssertUnwindSafe(|| {
                    __rt.block_on(async {
                        kithara_platform::time::timeout(__timeout_dur, async {
                            #inner_body
                        })
                        .await
                        .unwrap_or_else(|_| panic!(
                            "test `{}` timed out after {:?}",
                            #fn_name_str, __timeout_dur,
                        ))
                    })
                })
            );

            // Force-shutdown the runtime: never block on zombie blocking threads.
            __rt.shutdown_timeout(::std::time::Duration::from_millis(100));

            match __result {
                Ok(__v) => __v,
                #timeout_panic_arm
            }
        }
    }
}

/// Generate guarded env setup for the test body.
///
/// Uses a process-wide mutex to serialize env mutation and restores previous
/// values on drop to avoid cross-test leakage.
fn make_env_setup(env_vars: &[(String, String)]) -> TokenStream2 {
    if env_vars.is_empty() {
        return quote! {};
    }

    let mut actions: Vec<(String, Option<String>)> = env_vars
        .iter()
        .map(|(key, value)| (key.clone(), Some(value.clone())))
        .collect();

    // When NO_PROXY is requested, clear inherited proxy vars unless explicitly
    // overridden in the macro args.
    if env_vars
        .iter()
        .any(|(key, _)| key.eq_ignore_ascii_case("NO_PROXY"))
    {
        for key in [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
        ] {
            let is_explicit = env_vars.iter().any(|(env_key, _)| env_key == key);
            if !is_explicit {
                actions.push((key.to_string(), None));
            }
        }
    }

    let apply_actions: Vec<_> = actions
        .iter()
        .map(|(key, value)| {
            if let Some(value) = value {
                quote! {
                    __saved.push((#key, std::env::var(#key).ok()));
                    // SAFETY: guarded by process-wide env lock.
                    unsafe { std::env::set_var(#key, #value); }
                }
            } else {
                quote! {
                    __saved.push((#key, std::env::var(#key).ok()));
                    // SAFETY: guarded by process-wide env lock.
                    unsafe { std::env::remove_var(#key); }
                }
            }
        })
        .collect();

    quote! {
        #[cfg(not(target_arch = "wasm32"))]
        let _kithara_env_guard = {
            struct __KitharaEnvGuard {
                saved: ::std::vec::Vec<(
                    &'static str,
                    ::std::option::Option<::std::string::String>,
                )>,
                lock: ::std::option::Option<::std::sync::MutexGuard<'static, ()>>,
            }

            impl Drop for __KitharaEnvGuard {
                fn drop(&mut self) {
                    for (key, value) in self.saved.drain(..).rev() {
                        match value {
                            Some(value) => {
                                // SAFETY: test-scoped restoration under env lock.
                                unsafe { std::env::set_var(key, value); }
                            }
                            None => {
                                // SAFETY: test-scoped restoration under env lock.
                                unsafe { std::env::remove_var(key); }
                            }
                        }
                    }
                    let _ = self.lock.take();
                }
            }

            let __lock = kithara_platform::test_env_lock()
                .lock()
                .unwrap_or_else(|err| err.into_inner());

            let mut __saved = ::std::vec::Vec::new();
            #(#apply_actions)*

            __KitharaEnvGuard {
                saved: __saved,
                lock: Some(__lock),
            }
        };
    }
}

/// Wrap body in `catch_unwind`; re-panic unless the message matches a pattern.
///
/// Requires `futures` crate at the call site for async tests.
fn wrap_with_soft_fail(
    body: &TokenStream2,
    fn_name: &Ident,
    is_async: bool,
    patterns: &[String],
) -> TokenStream2 {
    let name_str = fn_name.to_string();
    let pattern_strs: Vec<_> = patterns.iter().map(|p| p.to_lowercase()).collect();
    let handle_panic = quote! {
        let __msg = if let Some(s) = __panic.downcast_ref::<&str>() {
            (*s).to_string()
        } else if let Some(s) = __panic.downcast_ref::<String>() {
            s.clone()
        } else {
            "unknown panic".to_string()
        };
        let __lower = __msg.to_lowercase();
        let __patterns: &[&str] = &[#(#pattern_strs),*];
        if __patterns.iter().any(|p| {
            __lower.contains(p)
                || (*p == "timeout" && __lower.contains("timed out"))
        }) {
            eprintln!("[SOFT FAIL] {}: {}", #name_str, __msg);
        } else {
            std::panic::resume_unwind(__panic);
        }
    };

    if is_async {
        quote! {
            {
                let __result = futures::FutureExt::catch_unwind(
                    std::panic::AssertUnwindSafe(async move #body)
                ).await;
                if let Err(__panic) = __result {
                    #handle_panic
                }
            }
        }
    } else {
        quote! {
            {
                let __result = std::panic::catch_unwind(
                    std::panic::AssertUnwindSafe(move || #body)
                );
                if let Err(__panic) = __result {
                    #handle_panic
                }
            }
        }
    }
}

/// Combine env-var setup and optional soft-fail wrapping around an
/// already-timeout-wrapped body.
fn finalize_body(
    inner: &TokenStream2,
    args: &TestArgs,
    fn_name: &Ident,
    is_async: bool,
) -> TokenStream2 {
    let env_setup = make_env_setup(&args.env_vars);
    if !args.soft_fail_patterns.is_empty() {
        let soft = wrap_with_soft_fail(inner, fn_name, is_async, &args.soft_fail_patterns);
        quote! { { #env_setup #soft } }
    } else if !args.env_vars.is_empty() {
        quote! { { #env_setup #inner } }
    } else {
        inner.clone()
    }
}

#[expect(clippy::too_many_arguments)]
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
    let tracing_init = make_tracing_init(args);
    let full = quote! { #tracing_init #preamble #(#body_stmts)* };
    let serial_attr = make_serial_attr(args);

    // Async + timeout on native: use manual runtime for clean shutdown.
    // This avoids #[tokio::test]'s Runtime::drop hanging on zombie
    // spawn_blocking threads after a timeout fires.
    if is_async && args.timeout.is_some() {
        let mut output = emit_async_timeout_test(
            fn_name,
            vis,
            ret_type,
            remaining_attrs,
            &full,
            args,
            &serial_attr,
        );
        // WASM side: async function with cooperative timeout
        let wasm_timeout = wrap_with_timeout(&full, &args.timeout, true, fn_name);
        let wasm_wrapped = finalize_body(&wasm_timeout, args, fn_name, true);
        output.extend(quote! {
            #(#remaining_attrs)*
            #[cfg(target_arch = "wasm32")]
            #[wasm_bindgen_test::wasm_bindgen_test]
            #vis async fn #fn_name() #ret_type #wasm_wrapped
        });
        return output;
    }

    // Async WITHOUT timeout: manual runtime (simple block_on, no watchdog).
    if is_async {
        let mut output = emit_async_runtime_test(
            fn_name,
            vis,
            ret_type,
            remaining_attrs,
            &full,
            args,
            &serial_attr,
        );
        // WASM side: plain async function
        let braced = quote! { { #full } };
        let wasm_wrapped = finalize_body(&braced, args, fn_name, true);
        output.extend(quote! {
            #(#remaining_attrs)*
            #[cfg(target_arch = "wasm32")]
            #[wasm_bindgen_test::wasm_bindgen_test]
            #vis async fn #fn_name() #ret_type #wasm_wrapped
        });
        return output;
    }

    // Sync tests: dual-platform attrs (#[test] + #[wasm_bindgen_test])
    let with_timeout = wrap_with_timeout(&full, &args.timeout, false, fn_name);
    let wrapped = finalize_body(&with_timeout, args, fn_name, false);

    quote! {
        #(#remaining_attrs)*
        #serial_attr
        #test_attrs
        #vis fn #fn_name() #ret_type #wrapped
    }
}

/// Emit a single browser test pair: WASM side (with `tokio::ensure_thread_pool`) and
/// optional native side. Returns one or two `#[cfg]`-gated functions.
#[expect(clippy::too_many_arguments)]
fn emit_browser_test(
    fn_name: &Ident,
    vis: &syn::Visibility,
    ret_type: &syn::ReturnType,
    remaining_attrs: &[&Attribute],
    is_async: bool,
    preamble: &TokenStream2,
    body_stmts: &[syn::Stmt],
    args: &TestArgs,
    browser_only: bool,
) -> TokenStream2 {
    let mut output = TokenStream2::new();
    let serial_attr = make_serial_attr(args);
    let tracing_init = make_tracing_init(args);

    output.extend(make_dedicated_worker_config());

    // WASM side: always async, with tokio::ensure_thread_pool init
    let wasm_body = quote! {
        #tracing_init
        kithara_platform::tokio::ensure_thread_pool().await;
        #preamble
        #(#body_stmts)*
    };
    let wasm_with_timeout = wrap_with_timeout(&wasm_body, &args.timeout, true, fn_name);
    let wasm_wrapped = finalize_body(&wasm_with_timeout, args, fn_name, true);
    output.extend(quote! {
        #(#remaining_attrs)*
        #[cfg(target_arch = "wasm32")]
        #[wasm_bindgen_test::wasm_bindgen_test]
        #vis async fn #fn_name() #ret_type #wasm_wrapped
    });

    // Native side (only for dual-platform: native+browser or tokio+browser)
    if !browser_only {
        let native_is_async = is_async || args.is_tokio;
        let native_body = quote! { #tracing_init #preamble #(#body_stmts)* };

        // Async + timeout: use manual runtime (same reason as emit_one_test)
        if native_is_async && args.timeout.is_some() {
            output.extend(emit_async_timeout_test(
                fn_name,
                vis,
                ret_type,
                remaining_attrs,
                &native_body,
                args,
                &serial_attr,
            ));
        } else if native_is_async {
            // Async without timeout: manual runtime
            output.extend(emit_async_runtime_test(
                fn_name,
                vis,
                ret_type,
                remaining_attrs,
                &native_body,
                args,
                &serial_attr,
            ));
        } else {
            // Sync native test
            let native_with_timeout =
                wrap_with_timeout(&native_body, &args.timeout, false, fn_name);
            let native_wrapped = finalize_body(&native_with_timeout, args, fn_name, false);
            output.extend(quote! {
                #(#remaining_attrs)*
                #serial_attr
                #[cfg(not(target_arch = "wasm32"))]
                #[test]
                #vis fn #fn_name() #ret_type #native_wrapped
            });
        }
    }

    output
}

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
struct GenCtx<'a> {
    fn_name: &'a Ident,
    ret_type: &'a syn::ReturnType,
    args: &'a TestArgs,
    vis: &'a syn::Visibility,
    remaining_attrs: &'a [&'a Attribute],
    cases: &'a [Case],
    params: &'a [ParamInfo],
    body_stmts: &'a [syn::Stmt],
    is_async: bool,
}

/// Case-name convention: `{fn_name}_{case.name}` or `{fn_name}_case_{i+1}`.
fn case_ident(fn_name: &Ident, case: &Case, index: usize) -> Ident {
    match &case.name {
        Some(name) => format_ident!("{}_{}", fn_name, name),
        None => format_ident!("{}_case_{}", fn_name, index + 1),
    }
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

fn emit_native_only_one(
    ctx: &GenCtx<'_>,
    name: &Ident,
    full: &TokenStream2,
    serial_attr: &TokenStream2,
    native_is_async: bool,
) -> TokenStream2 {
    let remaining_attrs = ctx.remaining_attrs;
    let vis = ctx.vis;
    let ret_type = ctx.ret_type;
    let args = ctx.args;

    if native_is_async && args.timeout.is_some() {
        return emit_async_timeout_test(
            name,
            vis,
            ret_type,
            remaining_attrs,
            full,
            args,
            serial_attr,
        );
    }
    if native_is_async {
        return emit_async_runtime_test(
            name,
            vis,
            ret_type,
            remaining_attrs,
            full,
            args,
            serial_attr,
        );
    }
    let with_timeout = wrap_with_timeout(full, &args.timeout, false, name);
    let wrapped = finalize_body(&with_timeout, args, name, false);
    quote! {
        #(#remaining_attrs)*
        #serial_attr
        #[cfg(not(target_arch = "wasm32"))]
        #[test]
        #vis fn #name() #ret_type #wrapped
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

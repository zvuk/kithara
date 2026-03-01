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
    is_native_only: bool,
    is_browser: bool,
    is_serial: bool,
    timeout: Option<Expr>,
    env_vars: Vec<(String, String)>,
    /// Substring patterns for soft-fail: if a panic message contains any of
    /// these (case-insensitive), the test prints a warning instead of failing.
    soft_fail_patterns: Vec<String>,
}

impl Parse for TestArgs {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let mut args = TestArgs {
            is_tokio: false,
            is_wasm_only: false,
            is_native_only: false,
            is_browser: false,
            is_serial: false,
            timeout: None,
            env_vars: Vec::new(),
            soft_fail_patterns: Vec::new(),
        };

        while !input.is_empty() {
            let ident: Ident = input.parse()?;
            match ident.to_string().as_str() {
                "tokio" => args.is_tokio = true,
                "wasm" => args.is_wasm_only = true,
                "native" => args.is_native_only = true,
                "browser" => args.is_browser = true,
                "serial" => args.is_serial = true,
                "timeout" => {
                    let content;
                    syn::parenthesized!(content in input);
                    args.timeout = Some(content.parse()?);
                }
                "env" => {
                    let content;
                    syn::parenthesized!(content in input);
                    while !content.is_empty() {
                        let key: Ident = content.parse()?;
                        content.parse::<Token![=]>()?;
                        let value: syn::LitStr = content.parse()?;
                        args.env_vars.push((key.to_string(), value.value()));
                        if !content.is_empty() {
                            content.parse::<Token![,]>()?;
                        }
                    }
                }
                "soft_fail" => {
                    let content;
                    syn::parenthesized!(content in input);
                    while !content.is_empty() {
                        let pattern: syn::LitStr = content.parse()?;
                        args.soft_fail_patterns.push(pattern.value());
                        if !content.is_empty() {
                            content.parse::<Token![,]>()?;
                        }
                    }
                    if args.soft_fail_patterns.is_empty() {
                        return Err(syn::Error::new(
                            ident.span(),
                            "soft_fail requires at least one pattern string",
                        ));
                    }
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

        if args.is_wasm_only && args.is_native_only {
            return Err(syn::Error::new(
                proc_macro2::Span::call_site(),
                "`wasm` and `native` are mutually exclusive",
            ));
        }

        if args.is_browser && args.is_wasm_only {
            return Err(syn::Error::new(
                proc_macro2::Span::call_site(),
                "`browser` and `wasm` are mutually exclusive (`browser` already implies wasm)",
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

fn make_serial_attr(args: &TestArgs) -> TokenStream2 {
    if args.is_serial {
        quote! { #[serial_test::serial] }
    } else {
        quote! {}
    }
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
#[allow(clippy::too_many_arguments)]
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

    quote! {
        #(#remaining_attrs)*
        #serial_attr
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

            let __rt = ::tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("tokio runtime");

            let __result = ::std::panic::catch_unwind(
                ::std::panic::AssertUnwindSafe(|| {
                    __rt.block_on(async {
                        ::tokio::time::timeout(__timeout_dur, async {
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

    let with_timeout = wrap_with_timeout(&full, &args.timeout, is_async, fn_name);
    let wrapped = finalize_body(&with_timeout, args, fn_name, is_async);
    let asyncness = is_async.then(|| quote! { async });

    quote! {
        #(#remaining_attrs)*
        #serial_attr
        #test_attrs
        #vis #asyncness fn #fn_name() #ret_type #wrapped
    }
}

// ---------------------------------------------------------------------------
// Browser test generation
// ---------------------------------------------------------------------------

/// Emit a single browser test pair: WASM side (with `ensure_thread_pool`) and
/// optional native side. Returns one or two `#[cfg]`-gated functions.
#[allow(clippy::too_many_arguments)]
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

    output.extend(make_dedicated_worker_config());

    // WASM side: always async, with ensure_thread_pool init
    let wasm_body = quote! {
        kithara_platform::ensure_thread_pool().await;
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
        let native_body = quote! { #preamble #(#body_stmts)* };

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
        } else {
            let native_attr = if native_is_async {
                quote! { #[tokio::test] }
            } else {
                quote! { #[test] }
            };
            let native_asyncness = native_is_async.then(|| quote! { async });
            let native_with_timeout =
                wrap_with_timeout(&native_body, &args.timeout, native_is_async, fn_name);
            let native_wrapped =
                finalize_body(&native_with_timeout, args, fn_name, native_is_async);
            output.extend(quote! {
                #(#remaining_attrs)*
                #serial_attr
                #[cfg(not(target_arch = "wasm32"))]
                #native_attr
                #vis #native_asyncness fn #fn_name() #ret_type #native_wrapped
            });
        }
    }

    output
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

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
/// #[kithara::test(soft_fail("connection", "refused"))] // soft-fail on matching panics
/// #[kithara::test(tokio, serial)]                      // serial execution (no parallel)
/// ```
///
/// ## `env`
///
/// `env(KEY = "value")` sets environment variables before the test body runs.
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
/// ## `browser`
///
/// The `browser` flag injects `kithara_platform::ensure_thread_pool().await` on
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
    let is_async = func.sig.asyncness.is_some();
    let vis = &func.vis;
    let fn_name = &func.sig.ident;
    let ret_type = &func.sig.output;
    let body_stmts = &func.block.stmts;

    // wasm-only: cfg(wasm32) + wasm_bindgen_test, no native counterpart.
    // Also emits `run_in_dedicated_worker` config so the test runner uses a
    // browser-based Web Worker instead of Node.js (required by wasm-bindgen-rayon).
    if args.is_wasm_only {
        let worker_config = make_dedicated_worker_config();
        if cases.is_empty() {
            let preamble = make_preamble(&params, None);
            let full = quote! { { #preamble #(#body_stmts)* } };
            let with_timeout = wrap_with_timeout(&full, &args.timeout, true, fn_name);
            let wrapped = finalize_body(&with_timeout, &args, fn_name, true);
            return Ok(quote! {
                #worker_config
                #(#remaining_attrs)*
                #[cfg(target_arch = "wasm32")]
                #[wasm_bindgen_test::wasm_bindgen_test]
                #vis async fn #fn_name() #ret_type #wrapped
            });
        }
        let mut tests = TokenStream2::new();
        tests.extend(worker_config);
        for (i, case) in cases.iter().enumerate() {
            let case_name = match &case.name {
                Some(name) => format_ident!("{}_{}", fn_name, name),
                None => format_ident!("{}_case_{}", fn_name, i + 1),
            };
            let preamble = make_preamble(&params, Some(&case.values));
            let full = quote! { { #preamble #(#body_stmts)* } };
            let with_timeout = wrap_with_timeout(&full, &args.timeout, true, &case_name);
            let wrapped = finalize_body(&with_timeout, &args, &case_name, true);
            tests.extend(quote! {
                #(#remaining_attrs)*
                #[cfg(target_arch = "wasm32")]
                #[wasm_bindgen_test::wasm_bindgen_test]
                #vis async fn #case_name() #ret_type #wrapped
            });
        }
        return Ok(tests);
    }

    // native-only (without browser): cfg(not(wasm32)) + #[test] or #[tokio::test]
    if args.is_native_only && !args.is_browser {
        let native_is_async = is_async || args.is_tokio;
        let serial_attr = make_serial_attr(&args);

        if cases.is_empty() {
            let preamble = make_preamble(&params, None);
            let full = quote! { #preamble #(#body_stmts)* };

            // Async + timeout: manual runtime
            if native_is_async && args.timeout.is_some() {
                return Ok(emit_async_timeout_test(
                    fn_name,
                    vis,
                    ret_type,
                    &remaining_attrs,
                    &full,
                    &args,
                    &serial_attr,
                ));
            }

            let test_attr = if native_is_async {
                quote! { #[tokio::test] }
            } else {
                quote! { #[test] }
            };
            let asyncness = native_is_async.then(|| quote! { async });
            let with_timeout = wrap_with_timeout(&full, &args.timeout, native_is_async, fn_name);
            let wrapped = finalize_body(&with_timeout, &args, fn_name, native_is_async);
            return Ok(quote! {
                #(#remaining_attrs)*
                #serial_attr
                #[cfg(not(target_arch = "wasm32"))]
                #test_attr
                #vis #asyncness fn #fn_name() #ret_type #wrapped
            });
        }

        let mut tests = TokenStream2::new();
        for (i, case) in cases.iter().enumerate() {
            let case_name = match &case.name {
                Some(name) => format_ident!("{}_{}", fn_name, name),
                None => format_ident!("{}_case_{}", fn_name, i + 1),
            };
            let preamble = make_preamble(&params, Some(&case.values));
            let full = quote! { #preamble #(#body_stmts)* };

            // Async + timeout: manual runtime (per case)
            if native_is_async && args.timeout.is_some() {
                tests.extend(emit_async_timeout_test(
                    &case_name,
                    vis,
                    ret_type,
                    &remaining_attrs,
                    &full,
                    &args,
                    &serial_attr,
                ));
                continue;
            }

            let test_attr = if native_is_async {
                quote! { #[tokio::test] }
            } else {
                quote! { #[test] }
            };
            let asyncness = native_is_async.then(|| quote! { async });
            let with_timeout = wrap_with_timeout(&full, &args.timeout, native_is_async, &case_name);
            let wrapped = finalize_body(&with_timeout, &args, &case_name, native_is_async);
            tests.extend(quote! {
                #(#remaining_attrs)*
                #serial_attr
                #[cfg(not(target_arch = "wasm32"))]
                #test_attr
                #vis #asyncness fn #case_name() #ret_type #wrapped
            });
        }
        return Ok(tests);
    }

    // browser: WASM with ensure_thread_pool init, optionally dual-platform
    //   browser alone      → wasm-only + init
    //   native, browser    → sync native + browser wasm
    //   tokio, browser     → async native + browser wasm
    if args.is_browser {
        let browser_only = !args.is_tokio && !args.is_native_only;
        if cases.is_empty() {
            let preamble = make_preamble(&params, None);
            return Ok(emit_browser_test(
                fn_name,
                vis,
                ret_type,
                &remaining_attrs,
                is_async,
                &preamble,
                body_stmts,
                &args,
                browser_only,
            ));
        }
        let mut tests = TokenStream2::new();
        for (i, case) in cases.iter().enumerate() {
            let case_name = match &case.name {
                Some(name) => format_ident!("{}_{}", fn_name, name),
                None => format_ident!("{}_case_{}", fn_name, i + 1),
            };
            let preamble = make_preamble(&params, Some(&case.values));
            tests.extend(emit_browser_test(
                &case_name,
                vis,
                ret_type,
                &remaining_attrs,
                is_async,
                &preamble,
                body_stmts,
                &args,
                browser_only,
            ));
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

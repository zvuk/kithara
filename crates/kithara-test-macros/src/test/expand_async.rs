use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{Attribute, Ident};

use super::{
    parse::TestArgs,
    shared::{
        finalize_body, make_ambient_stmt, make_dedicated_worker_config, make_hang_budget,
        make_hard_timeout_watchdog, make_prekill_guard, make_runtime_builder, make_selenium_attrs,
        make_serial_attr, make_tracing_init, make_wasm_serial_guard, wrap_with_model,
        wrap_with_soft_fail, wrap_with_timeout,
    },
};

/// Emit a native async test with a **manual tokio runtime** (no timeout).
///
/// Generates a sync `#[test]` fn that creates a `current_thread` runtime via
/// `kithara_platform::tokio::runtime::Builder` and calls `block_on`.
///
/// This avoids depending on `#[tokio::test]` so consumer crates do not need
/// `tokio` as a direct dependency.
pub(crate) fn emit_async_runtime_test(
    fn_name: &Ident,
    vis: &syn::Visibility,
    ret_type: &syn::ReturnType,
    remaining_attrs: &[&Attribute],
    body: &TokenStream2,
    args: &TestArgs,
    serial_attr: &TokenStream2,
) -> TokenStream2 {
    let hang_budget = make_hang_budget(args.hang_timeout_secs);
    let prekill_guard = make_prekill_guard(fn_name);
    let selenium_attr = make_selenium_attrs(args);
    let runtime_builder = make_runtime_builder(args);
    let flash = args.flash.unwrap_or(true);
    let fn_name_str = fn_name.to_string();
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

    // Installing the subscriber parses filter directives and allocates. The
    // real-time sanitizer checks every poll of the test future, and the first
    // test to reach this — whichever it happens to be — was reported as an
    // unsafe `malloc` in a real-time context. Process-wide setup belongs
    // before the future exists.
    let tracing_init = make_tracing_init(args, remaining_attrs);
    let runtime_body = quote! {
        {
            #tracing_init
            let __rt = #runtime_builder;
            __rt.block_on(
                ::kithara_test_utils::kithara_platform::flash::participate(
                    ::kithara_test_utils::kithara_platform::flash::with_ambient(
                        #flash,
                        ::kithara_test_utils::no_block::watch_root(
                            #fn_name_str,
                            async {
                                #inner_body
                            },
                        ),
                    ),
                    ::core::panic::Location::caller(),
                ),
            )
        }
    };
    let runtime_body = wrap_with_model(&runtime_body, args);

    quote! {
        #(#remaining_attrs)*
        #serial_attr
        #selenium_attr
        #[cfg(not(target_arch = "wasm32"))]
        #[test]
        #vis fn #fn_name() #ret_type {
            #hang_budget
            #prekill_guard
            #runtime_body
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
pub(crate) fn emit_async_timeout_test(
    fn_name: &Ident,
    vis: &syn::Visibility,
    ret_type: &syn::ReturnType,
    remaining_attrs: &[&Attribute],
    body: &TokenStream2,
    args: &TestArgs,
    serial_attr: &TokenStream2,
) -> TokenStream2 {
    let dur = args
        .timeout
        .as_ref()
        .expect("BUG: caller checks `args.timeout.is_some()` before this fn");
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

    let braced = quote! { { #body } };
    let inner_body = if !args.soft_fail_patterns.is_empty() {
        wrap_with_soft_fail(&braced, fn_name, true, &args.soft_fail_patterns)
    } else {
        braced
    };

    let hang_budget = make_hang_budget(args.hang_timeout_secs);
    let hard_timeout_watchdog = make_hard_timeout_watchdog(&fn_name_str);
    let selenium_attr = make_selenium_attrs(args);
    let runtime_builder = make_runtime_builder(args);
    let flash = args.flash.unwrap_or(true);

    // Installing the subscriber parses filter directives and allocates. The
    // real-time sanitizer checks every poll of the test future, and the first
    // test to reach this — whichever it happens to be — was reported as an
    // unsafe `malloc` in a real-time context. Process-wide setup belongs
    // before the future exists.
    let tracing_init = make_tracing_init(args, remaining_attrs);
    let runtime_body = quote! {
        {
            #tracing_init
            let __rt = #runtime_builder;
            let __result = ::std::panic::catch_unwind(
                ::std::panic::AssertUnwindSafe(|| {
                    __rt.block_on(
                        ::kithara_test_utils::kithara_platform::flash::participate(
                            async {
                                    ::kithara_test_utils::kithara_platform::time::timeout(
                                        __timeout_dur,
                                        ::kithara_test_utils::kithara_platform::flash::with_ambient(
                                            #flash,
                                            ::kithara_test_utils::no_block::watch_root(
                                                #fn_name_str,
                                                async {
                                                    #inner_body
                                                },
                                            ),
                                        ),
                                    )
                                    .await
                                    .unwrap_or_else(|_| {
                                        let __timeout_diagnostic = format!(
                                            "test `{}` timed out after {:?}",
                                            #fn_name_str, __timeout_dur,
                                        );
                                        ::kithara_test_utils::hang::record_test_hang(
                                            "wall-timeout",
                                            &__timeout_diagnostic,
                                        );
                                        panic!("{}", __timeout_diagnostic)
                                    })
                            },
                            ::core::panic::Location::caller(),
                        ),
                    )
                })
            );

            __rt.shutdown_timeout(::std::time::Duration::from_millis(100));

            match __result {
                Ok(__v) => __v,
                #timeout_panic_arm
            }
        }
    };
    let runtime_body = wrap_with_model(&runtime_body, args);

    quote! {
        #(#remaining_attrs)*
        #serial_attr
        #selenium_attr
        #[cfg(not(target_arch = "wasm32"))]
        #[test]
        #vis fn #fn_name() #ret_type {
            #hang_budget

            let __timeout_dur: ::std::time::Duration = #dur;

            let __done = ::kithara_test_utils::kithara_platform::sync::Arc::new(
                ::std::sync::atomic::AtomicBool::new(false),
            );
            #hard_timeout_watchdog
            struct __WG(
                ::kithara_test_utils::kithara_platform::sync::Arc<
                    ::std::sync::atomic::AtomicBool,
                >,
            );
            impl Drop for __WG {
                fn drop(&mut self) {
                    self.0.store(true, ::std::sync::atomic::Ordering::SeqCst);
                }
            }
            let _wg = __WG(__done);
            #runtime_body
        }
    }
}

/// Emit a single browser test pair: WASM side (with `tokio::ensure_thread_pool`) and
/// optional native side. Returns one or two `#[cfg]`-gated functions.
#[expect(clippy::too_many_arguments)]
pub(crate) fn emit_browser_test(
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
    let tracing_init = make_tracing_init(args, remaining_attrs);

    output.extend(make_dedicated_worker_config());

    // wasm emission: no per-poll `with_ambient`, the body-held scope is the
    // sole ambient writer — KEEP it (same for the native sync branch below).
    let ambient = make_ambient_stmt(args);
    let wasm_body = quote! {
        #tracing_init
        ::kithara_test_utils::kithara_platform::tokio::ensure_thread_pool().await;
        #preamble
        #ambient
        #(#body_stmts)*
    };
    let wasm_with_timeout = wrap_with_timeout(&wasm_body, &args.timeout, true, fn_name);
    let wasm_wrapped = finalize_body(&wasm_with_timeout, args, fn_name, true);
    let wasm_serial_guard = make_wasm_serial_guard(args);
    output.extend(quote! {
        #(#remaining_attrs)*
        #[cfg(target_arch = "wasm32")]
        #[wasm_bindgen_test::wasm_bindgen_test]
        #vis async fn #fn_name() #ret_type {
            #wasm_serial_guard
            #wasm_wrapped
        }
    });

    if !browser_only {
        let native_is_async = is_async || args.is_tokio;
        // Plain body for the async-native branches: their sole ambient holder
        // is the per-poll `with_ambient` inside the emitted runtime wrapper.
        // Tracing is not set up here — see `emit_async_runtime_test`.
        let native_body = quote! { #preamble #(#body_stmts)* };

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
            let native_body_held = quote! { #tracing_init #preamble #ambient #(#body_stmts)* };
            let native_with_timeout =
                wrap_with_timeout(&native_body_held, &args.timeout, false, fn_name);
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

#[cfg(test)]
mod tests {
    use proc_macro2::TokenStream as TokenStream2;
    use quote::quote;
    use syn::{Ident, ReturnType, Visibility, parse_quote};

    use super::{TestArgs, emit_async_timeout_test};

    #[test]
    fn native_timeout_emitter_records_wall_and_hard_hangs() -> syn::Result<()> {
        let args =
            syn::parse_str::<TestArgs>("tokio, timeout(::std::time::Duration::from_secs(5))")?;
        let name: Ident = parse_quote!(stress_case);
        let visibility = Visibility::Inherited;
        let return_type = ReturnType::Default;
        let attributes = [];
        let body = quote!({});
        let serial = TokenStream2::new();

        let expanded = emit_async_timeout_test(
            &name,
            &visibility,
            &return_type,
            &attributes,
            &body,
            &args,
            &serial,
        )
        .to_string();

        assert!(!expanded.contains("PreKillGuard"));
        assert!(expanded.contains("record_test_hang"));
        assert!(expanded.contains("wall-timeout"));
        assert!(expanded.contains("hard-timeout"));
        Ok(())
    }
}

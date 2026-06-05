use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{Attribute, Ident};

use super::{
    parse::TestArgs,
    shared::{
        finalize_body, make_dedicated_worker_config, make_env_setup, make_real_time_hint,
        make_runtime_builder, make_selenium_attrs, make_serial_attr, make_tracing_init,
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
    let env_setup = make_env_setup(&args.env_vars);
    let selenium_attr = make_selenium_attrs(args);
    let runtime_builder = make_runtime_builder(args);
    let real_time_hint = make_real_time_hint(args);
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
            let __probe_install_id =
                ::kithara_test_utils::probe::bump_install_id();
            __rt.block_on(
                // Wrap the root task in the quiescence poll-wrapper so the test
                // driver counts as a running participant while polled (identity
                // off the sim path). Without this the virtual clock would race
                // past the driver's own work between awaits — the worker's idle
                // timer advances the clock while the driver is between polls, so
                // a `sleep`/`read` loop times out on a deadline that already
                // jumped. Mirrors `emit_async_timeout_test`.
                kithara_platform::time::participate(
                    ::kithara_test_utils::probe::OWNED_INSTALL_ID
                        .scope(__probe_install_id, async {
                            #real_time_hint
                            #inner_body
                        }),
                ),
            )
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

    let env_setup = make_env_setup(&args.env_vars);
    let selenium_attr = make_selenium_attrs(args);
    let runtime_builder = make_runtime_builder(args);
    let real_time_hint = make_real_time_hint(args);

    quote! {
        #(#remaining_attrs)*
        #serial_attr
        #selenium_attr
        #[cfg(not(target_arch = "wasm32"))]
        #[test]
        #vis fn #fn_name() #ret_type {
            #env_setup

            let __timeout_dur: ::std::time::Duration = #dur;

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
            let __probe_install_id =
                ::kithara_test_utils::probe::bump_install_id();

            let __result = ::std::panic::catch_unwind(
                ::std::panic::AssertUnwindSafe(|| {
                    __rt.block_on(
                        // Wrap the root task in the quiescence poll-wrapper so the
                        // test driver counts as a running participant while polled
                        // (identity off the sim path). Without this the virtual
                        // clock would race past the driver's own work between awaits.
                        kithara_platform::time::participate(
                            ::kithara_test_utils::probe::OWNED_INSTALL_ID.scope(
                                __probe_install_id,
                                async {
                                    #real_time_hint
                                    // Wall-clock safety net: must fire on REAL
                                    // time even under `flash-time` (a hung test
                                    // hangs real time too).
                                    kithara_platform::time::timeout(__timeout_dur, async {
                                        #inner_body
                                    })
                                    .await
                                    .unwrap_or_else(|_| panic!(
                                        "test `{}` timed out after {:?}",
                                        #fn_name_str, __timeout_dur,
                                    ))
                                },
                            ),
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
    let tracing_init = make_tracing_init(args);

    output.extend(make_dedicated_worker_config());

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

    if !browser_only {
        let native_is_async = is_async || args.is_tokio;
        let native_body = quote! { #tracing_init #preamble #(#body_stmts)* };

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

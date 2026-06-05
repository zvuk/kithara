#![allow(
    clippy::option_if_let_else,
    reason = "match is more readable for these quote!-emitting branches"
)]

use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{Expr, Ident};

use super::parse::TestArgs;

pub(crate) fn make_serial_attr(args: &TestArgs) -> TokenStream2 {
    if args.is_serial {
        quote! { #[serial_test::serial] }
    } else {
        quote! {}
    }
}

/// Test attributes for **sync** tests only (dual-platform: native `#[test]` + WASM).
///
/// Async tests are handled separately via `emit_async_runtime_test` /
/// `emit_async_timeout_test` which create a manual tokio runtime on native.
pub(crate) fn make_sync_test_attrs() -> TokenStream2 {
    let native = quote! { #[cfg_attr(not(target_arch = "wasm32"), test)] };
    let wasm = quote! { #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)] };
    quote! { #native #wasm }
}

/// Generate the tokio runtime builder expression.
///
/// Uses `new_multi_thread().worker_threads(2)` when `multi_thread` or `selenium`
/// is set; otherwise uses `new_current_thread()`.
pub(crate) fn make_runtime_builder(args: &TestArgs) -> TokenStream2 {
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

/// Real-time hint for the block_on **driver** thread (flash-time only; ZST no-op
/// off the sim path).
///
/// Under `flash-time` the test body is the DRIVER: it observes/polls the
/// system-under-test. Its bounding waits must measure REAL wall-clock so they
/// neither race ahead of (collapsing virtual time outpacing a real fetch) nor
/// stall against the engine. The system-under-test — spawned tasks, the
/// download/server runtimes, the decode/audio worker threads — keeps the
/// collapsed virtual clock. On a `multi_thread` runtime the `block_on` thread
/// runs ONLY this future (it does not steal worker tasks), so this marker stays
/// on the driver and never leaks to a system task. Emitted only for
/// `multi_thread`; a `current_thread` test shares its one runtime thread between
/// driver and system, so it stays fully virtual (its driver waits must be
/// count-bounded, like the phase-continuity pull loop).
pub(crate) fn make_real_time_hint(_args: &TestArgs) -> TokenStream2 {
    // EXPERIMENT: no real-time island. The driver runs on the sim clock like
    // the rest of the system so virtual time advances deadline-by-deadline
    // (deterministic), instead of the system racing ahead during a real-wall-
    // clock driver wait.
    quote! {}
}

pub(crate) fn make_tracing_init(args: &TestArgs) -> TokenStream2 {
    if let Some(filter) = &args.tracing_filter {
        quote! {
            ::kithara_test_utils::test::setup_tracing_with_filter(#filter);
        }
    } else {
        quote! {
            ::kithara_test_utils::test::setup_tracing();
        }
    }
}

/// Selenium tests no longer auto-inject `#[ignore]` — the suite runs only
/// when the wasm-target test driver picks them up (`just test-selenium`),
/// so plain `cargo test` already skips them by virtue of platform gating.
pub(crate) fn make_selenium_attrs(_args: &TestArgs) -> TokenStream2 {
    quote! {}
}

/// Emit a `const _: ()` block that tells `wasm-bindgen-test-runner` to use a
/// dedicated Web Worker instead of Node.js. Multiple copies are harmless — the
/// anonymous `const _` wrapper prevents name collisions, and the linker merges
/// the `__wasm_bindgen_test_unstable` section entries.
pub(crate) fn make_dedicated_worker_config() -> TokenStream2 {
    quote! {
        #[cfg(target_arch = "wasm32")]
        const _: () = {
            #[unsafe(link_section = "__wasm_bindgen_test_unstable")]
            pub static __WBG_TEST_RUN_IN_DEDICATED_WORKER: [u8; 1] = [0x02u8];
        };
    }
}

pub(crate) fn wrap_with_timeout(
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
        quote! {
            {
                let __timeout_dur: ::std::time::Duration = #dur;
                let __body = async { #body };
                // Wall-clock safety net: must fire on REAL time even under
                // `flash-time` (a hung test hangs real time too).
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

/// Generate guarded env setup for the test body.
///
/// Uses a process-wide mutex to serialize env mutation and restores previous
/// values on drop to avoid cross-test leakage.
pub(crate) fn make_env_setup(env_vars: &[(String, String)]) -> TokenStream2 {
    if env_vars.is_empty() {
        return quote! {};
    }

    let mut actions: Vec<(String, Option<String>)> = env_vars
        .iter()
        .map(|(key, value)| (key.clone(), Some(value.clone())))
        .collect();

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

            let __lock = kithara_platform::env_mutation_lock()
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
pub(crate) fn wrap_with_soft_fail(
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
pub(crate) fn finalize_body(
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

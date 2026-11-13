#![allow(
    clippy::option_if_let_else,
    reason = "match is more readable for these quote!-emitting branches"
)]

use kithara_test_args::{TestArgs, TestFlags};
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{Attribute, Ident, ext::IdentExt as _};

pub(crate) fn make_serial_attr(args: &TestArgs) -> TokenStream2 {
    if args.flags().contains(TestFlags::SERIAL) {
        quote! {
            #[::kithara_test_utils::serial_test::serial(crate = ::kithara_test_utils::serial_test)]
        }
    } else {
        quote! {}
    }
}

/// WASM counterpart of [`make_serial_attr`]: `serial_test` drives native
/// threads, which a browser test binary does not have, so the body takes an
/// async lock instead. The lock is declared right here, next to the `await`
/// that takes it — it has one caller and needs no home of its own.
pub(crate) fn make_wasm_serial_guard(args: &TestArgs) -> TokenStream2 {
    if args.flags().contains(TestFlags::SERIAL) {
        quote! {
            static KITHARA_WASM_SERIAL:
                ::kithara_test_utils::kithara_platform::AsyncMutex<()> =
                ::kithara_test_utils::kithara_platform::AsyncMutex::new(());
            let _kithara_wasm_serial_guard = KITHARA_WASM_SERIAL.lock().await;
        }
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
///
/// Under Miri the runtime gets a timer and no reactor. `enable_all` builds the
/// I/O driver, whose first act is the host's event-loop syscall - `kqueue` on
/// macOS, `epoll_create1` on Linux - and Miri answers that with
/// "unsupported operation", aborting the whole test binary at the first async
/// test. The crates the Miri lane interprets hold atomics rather than sockets,
/// so the timer is what they use and the reactor is what they can go without.
pub(crate) fn make_runtime_builder(args: &TestArgs) -> TokenStream2 {
    // Each constructor is taken on its own line: they return the builder by
    // value and every method after them borrows it, so naming one expression
    // that ends in a method call would bind a reference to a temporary.
    let (base, threads) = if args.flags().contains(TestFlags::MULTI_THREAD) {
        (
            quote! {
                ::kithara_test_utils::kithara_platform::tokio::runtime::Builder::new_multi_thread()
            },
            quote! { builder.worker_threads(2); },
        )
    } else {
        (
            quote! {
                ::kithara_test_utils::kithara_platform::tokio::runtime::Builder::new_current_thread()
            },
            TokenStream2::new(),
        )
    };
    quote! {
        {
            let mut builder = #base;
            #threads
            #[cfg(not(miri))]
            builder.enable_all();
            #[cfg(miri)]
            builder.enable_time();
            builder.build().expect("kithara test runtime")
        }
    }
}

/// Body-held flash mode — ONLY for emit paths without a per-poll
/// `with_ambient` wrapper (native sync, wasm), where it is the sole ambient
/// writer of the body. The active guard follows ambient so ordinary platform
/// time calls select the virtual clock. Async-native emissions must NOT carry
/// these guards: a body-held scope inside the cancellable timeout tears down
/// non-LIFO on `Elapsed`.
pub(crate) fn make_ambient_stmt(args: &TestArgs) -> TokenStream2 {
    let flash = args.flash().unwrap_or(true);
    quote! {
        let __flash_ambient =
            ::kithara_test_utils::kithara_platform::flash::ambient_scope(#flash);
        let __flash_active =
            ::kithara_test_utils::kithara_platform::flash::enter_dynamic(#flash);
    }
}

pub(crate) fn make_clock_measurement_stmt(args: &TestArgs, fn_name: &Ident) -> TokenStream2 {
    let name = fn_name.unraw().to_string();
    let flash = args.flash().unwrap_or(true);
    quote! {
        #[cfg(not(target_arch = "wasm32"))]
        let __kithara_clock_measurement =
            ::kithara_test_utils::test::ClockMeasurement::start(
                concat!(module_path!(), "::", #name),
                #flash,
            );
    }
}

/// Native synchronous statements own credit after their ambient scope is installed.
pub(crate) fn wrap_with_sync_participation(body: &TokenStream2, args: &TestArgs) -> TokenStream2 {
    if args.flags().contains(TestFlags::LOOM) || args.flash() == Some(false) {
        return body.clone();
    }
    quote! {
        #[cfg(not(target_arch = "wasm32"))]
        {
            ::kithara_test_utils::kithara_platform::flash::participate_sync(|| { #body })
        }
        #[cfg(target_arch = "wasm32")]
        { #body }
    }
}

/// Per-test process setup, spliced as statements into the test body's scope:
/// the log subscriber, then the `load` feature's CPU contention held until
/// the body ends.
pub(crate) fn make_test_setup(args: &TestArgs, remaining_attrs: &[&Attribute]) -> TokenStream2 {
    let init = if let Some(filter) = args.tracing_filter() {
        quote! {
            ::kithara_test_utils::test::setup_tracing_with_filter(#filter);
        }
    } else {
        quote! {
            ::kithara_test_utils::test::setup_tracing();
        }
    };
    // `#[should_panic]` makes the panic the contract: the panic-dump hook must
    // not record it as evidence.
    let expected_panic = expects_panic(remaining_attrs).then(|| {
        quote! {
            #[cfg(not(target_arch = "wasm32"))]
            ::kithara_test_utils::hang::suppress_expected_panic_dumps();
        }
    });
    quote! {
        #init
        #expected_panic
        #[cfg(not(target_arch = "wasm32"))]
        let _kithara_test_load = ::kithara_test_utils::load::LoadGuard::start();
    }
}

/// The test declares its panic (`#[should_panic]`, bare or inside `cfg_attr`).
/// Token-level matching keeps the `cfg_attr` form covered without evaluating
/// its condition: suppressing on a platform where the attribute is inactive
/// only skips a dump for a test that then does not panic.
fn expects_panic(remaining_attrs: &[&Attribute]) -> bool {
    use quote::ToTokens as _;
    remaining_attrs.iter().any(|attr| {
        attr.path().is_ident("should_panic")
            || (attr.path().is_ident("cfg_attr")
                && attr.to_token_stream().to_string().contains("should_panic"))
    })
}

pub(crate) fn make_prekill_guard(fn_name: &Ident) -> TokenStream2 {
    let fn_name = fn_name.to_string();
    quote! {
        #[cfg(not(target_arch = "wasm32"))]
        let _kithara_prekill_guard =
            ::kithara_test_utils::hang::PreKillGuard::new(#fn_name);
    }
}

pub(crate) fn wrap_with_model(
    body: &TokenStream2,
    args: &TestArgs,
    fn_name: &Ident,
) -> TokenStream2 {
    if !args.flags().contains(TestFlags::LOOM) {
        return body.clone();
    }
    let clock_measurement = make_clock_measurement_stmt(args, fn_name);
    quote! {
        {
            ::kithara_test_utils::kithara_platform::__private::model(move || {
                #clock_measurement
                #body
            })
        }
    }
}

/// Selenium tests no longer auto-inject `#[ignore]` — the suite runs only
/// when the wasm-target test driver picks them up
/// (`just test run --lane=selenium-firefox`),
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
    args: &TestArgs,
    is_async: bool,
    fn_name: &Ident,
) -> TokenStream2 {
    let fn_name_str = fn_name.to_string();
    let dur = super::timeout::resolve(args);
    let prekill_guard = make_prekill_guard(fn_name);

    if is_async {
        quote! {
            {
                #prekill_guard
                let __timeout_dur: ::std::time::Duration = #dur;
                let __body = async { #body };
                // Wall-clock safety net: must fire on REAL time even under
                // `flash` (a hung test hangs real time too).
                ::kithara_test_utils::kithara_platform::time::timeout(__timeout_dur, __body)
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
            }
        }
    } else {
        quote! {
            {
                #prekill_guard
                let __timeout_dur: ::std::time::Duration = #dur;
                // Consume the closure at its call site so FnMut needs no mutable binding.
                let __body = move || { #body };
                #[cfg(not(target_arch = "wasm32"))]
                {
                    let (tx, rx) = ::std::sync::mpsc::channel();
                    let handle = ::std::thread::spawn(move || {
                        tx.send(({ __body })()).ok();
                    });
                    match rx.recv_timeout(__timeout_dur) {
                        Ok(v) => {
                            handle.join().ok();
                            v
                        }
                        Err(::std::sync::mpsc::RecvTimeoutError::Timeout) => {
                            let __timeout_diagnostic = format!(
                                "test `{}` timed out after {:?}",
                                #fn_name_str, __timeout_dur,
                            );
                            ::kithara_test_utils::hang::record_test_hang(
                                "sync-timeout",
                                &__timeout_diagnostic,
                            );
                            panic!("{}", __timeout_diagnostic)
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
                    let __started = ::kithara_test_utils::kithara_platform::time::WallInstant::now();
                    let __result = ({ __body })();
                    if __started.elapsed() > __timeout_dur {
                        let __diagnostic = format!(
                            "test `{}` timed out after {:?}",
                            #fn_name_str, __timeout_dur,
                        );
                        ::kithara_test_utils::hang::record_test_hang("sync-timeout", &__diagnostic);
                        panic!("{}", __diagnostic);
                    }
                    __result
                }
            }
        }
    }
}

/// Arm this test's hang-watchdog budget for as long as the guard lives.
///
/// One process-global atomic rather than `KITHARA_HANG_TIMEOUT_SECS`: a watched
/// function can be entered from a spawned worker, so the budget must be visible
/// process-wide, and mutating the environment for that is undefined behaviour
/// while any other thread reads it.
pub(crate) fn make_hang_budget(secs: Option<u64>) -> TokenStream2 {
    secs.map_or_else(TokenStream2::new, |secs| {
        quote! {
            let __kithara_hang_budget = ::kithara_test_utils::hang::override_timeout(
                ::kithara_test_utils::kithara_platform::time::Duration::from_secs(#secs),
            );
        }
    })
}

/// Backstop for a test whose runtime never shuts down: a plain thread that
/// sleeps past the deadline and aborts the process if the guard has not fired.
///
/// Emitted for every target but Miri. The thread counts real seconds against a
/// body Miri interprets some hundred times slower than it would execute, and
/// the way it reports an overrun is `abort()`, which ends the whole test binary
/// rather than the one test. Under Miri the lane's own job timeout is the
/// backstop.
///
/// Reads `__timeout_dur` and `__done` from the scope it is emitted into.
pub(crate) fn make_hard_timeout_watchdog(fn_name: &str) -> TokenStream2 {
    quote! {
        #[cfg(not(miri))]
        {
            let __done_w = __done.clone();
            let __fn = #fn_name;
            ::std::thread::spawn(move || {
                ::std::thread::sleep(
                    __timeout_dur + ::std::time::Duration::from_secs(3),
                );
                if !__done_w.load(::std::sync::atomic::Ordering::SeqCst) {
                    let __timeout_diagnostic = format!(
                        "test `{}` exceeded {:?} (runtime shutdown blocked)",
                        __fn, __timeout_dur,
                    );
                    eprintln!(
                        "\n\x1b[1;31mHARD TIMEOUT\x1b[0m: {}. Aborting process.\n",
                        __timeout_diagnostic,
                    );
                    ::kithara_test_utils::hang::record_test_hang(
                        "hard-timeout",
                        &__timeout_diagnostic,
                    );
                    ::std::process::abort();
                }
            });
        }
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

/// Combine native clock measurement, hang budget and optional soft-fail wrapping.
pub(crate) fn finalize_body(
    inner: &TokenStream2,
    args: &TestArgs,
    fn_name: &Ident,
    is_async: bool,
) -> TokenStream2 {
    let hang_budget = make_hang_budget(args.hang_timeout_secs());
    let clock_measurement = if args.flags().contains(TestFlags::LOOM) {
        TokenStream2::new()
    } else {
        make_clock_measurement_stmt(args, fn_name)
    };
    let body = if args.soft_fail_patterns().is_empty() {
        inner.clone()
    } else {
        wrap_with_soft_fail(inner, fn_name, is_async, args.soft_fail_patterns())
    };
    quote! { { #hang_budget #clock_measurement #body } }
}

#[cfg(test)]
mod tests {
    use kithara_test_args::TestArgs;
    use quote::quote;
    use syn::{Ident, parse_quote};

    use super::wrap_with_timeout;

    #[test]
    fn default_timeout_native_body_installs_prekill_guard() {
        let name: Ident = parse_quote!(stress_case);

        let expanded =
            wrap_with_timeout(&quote!({}), &TestArgs::default(), false, &name).to_string();

        assert!(expanded.contains("PreKillGuard"));
        assert!(expanded.contains("stress_case"));
    }

    #[test]
    fn sync_timeout_records_durable_evidence_before_panicking() {
        let name: Ident = parse_quote!(stress_case);
        let mut args = TestArgs::default();
        args.set_timeout(Some(parse_quote!(::std::time::Duration::from_secs(5))));

        let expanded = wrap_with_timeout(&quote!({}), &args, false, &name).to_string();

        assert!(expanded.contains("record_test_hang"));
        assert!(expanded.contains("sync-timeout"));
        assert!(expanded.contains("timed out after"));
    }

    #[test]
    fn async_timeout_records_durable_evidence_before_panicking() {
        let name: Ident = parse_quote!(stress_case);
        let mut args = TestArgs::default();
        args.set_timeout(Some(parse_quote!(::std::time::Duration::from_secs(5))));

        let expanded = wrap_with_timeout(&quote!({}), &args, true, &name).to_string();

        assert!(expanded.contains("record_test_hang"));
        assert!(expanded.contains("wall-timeout"));
        assert!(expanded.contains("timed out after"));
    }
}

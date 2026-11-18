#![allow(
    clippy::option_if_let_else,
    reason = "match is more readable for these quote!-emitting branches"
)]

use kithara_test_args::{TestArgs, TestFlags};
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{Attribute, Ident, ReturnType, ext::IdentExt as _};

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
    ret_type: &ReturnType,
    entry_ret: &ReturnType,
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
        let ret_ty = match ret_type {
            ReturnType::Default => quote! { () },
            ReturnType::Type(_, ty) => quote! { #ty },
        };
        let convert = exit_code_to_ret(entry_ret);
        quote! {
            {
                #prekill_guard
                let __timeout_dur: ::std::time::Duration = #dur;
                // Consume the closure at its call site so FnMut needs no mutable binding.
                let __body = move || { #body };
                #[cfg(not(target_arch = "wasm32"))]
                {
                    let (tx, rx) = ::std::sync::mpsc::channel();
                    let mut __thread_builder = ::std::thread::Builder::new();
                    if let Some(__thread_name) = ::std::thread::current().name() {
                        __thread_builder = __thread_builder.name(__thread_name.to_owned());
                    }
                    let handle = __thread_builder
                        .spawn(move || {
                            // The body result is reported inside the worker via
                            // `Termination::report` (needs only `Debug` on the
                            // error type), so a non-`Send` error never crosses
                            // the thread boundary. A body panic stays a panic
                            // payload; a non-success `ExitCode` stays a failure
                            // value and is converted back by the test thread.
                            let __outcome = ::std::panic::catch_unwind(
                                ::std::panic::AssertUnwindSafe(|| {
                                    let __result: #ret_ty = ({ __body })();
                                    ::std::process::Termination::report(__result)
                                }),
                            );
                            let _ = tx.send(__outcome);
                        })
                        .expect("kithara sync test thread");
                    match rx.recv_timeout(__timeout_dur) {
                        Ok(Ok(__code)) => {
                            handle.join().ok();
                            #convert
                        }
                        Ok(Err(__payload)) => {
                            handle.join().ok();
                            ::std::panic::resume_unwind(__payload)
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

fn is_unit_type(ty: &syn::Type) -> bool {
    matches!(ty, syn::Type::Tuple(t) if t.elems.is_empty())
}
fn is_exit_code_type(ty: &syn::Type) -> bool {
    match ty {
        syn::Type::Path(p) => p
            .path
            .segments
            .last()
            .is_some_and(|s| s.ident == "ExitCode"),
        _ => false,
    }
}

fn is_never_type(ty: &syn::Type) -> bool {
    matches!(ty, syn::Type::Never(_))
}

fn result_ok_type(ty: &syn::Type) -> Option<&syn::Type> {
    let syn::Type::Path(p) = ty else {
        return None;
    };
    let seg = p.path.segments.last()?;
    if seg.ident != "Result" {
        return None;
    }
    let syn::PathArguments::AngleBracketed(args) = &seg.arguments else {
        return None;
    };
    // `Result<T, E>` and single-argument aliases (`syn::Result<T>`,
    // `anyhow::Result<T>`) both put the Ok type first.
    match args.args.first()? {
        syn::GenericArgument::Type(ok_ty) => Some(ok_ty),
        _ => None,
    }
}

/// The return type of a native sync entry that reports a body failure as an
/// `ExitCode` value (used by `soft_fail`, which must not see the failure as a
/// panic).
pub(crate) fn exit_code_ret() -> ReturnType {
    syn::parse_quote!(-> ::std::process::ExitCode)
}

/// Tokens converting a `Termination::report` `__code: ExitCode` back into the
/// entry's declared return type.
///
/// A non-success code is a body failure, not a panic. For a `()`/`Result`
/// entry it must become a panic here (the body error was already printed by
/// `Termination::report`), while an `ExitCode` entry passes the code through
/// untouched so `soft_fail` sees the failure as a value, not a panic.
fn exit_code_to_ret(entry_ret: &ReturnType) -> TokenStream2 {
    let fail = quote! { panic!("test body returned a failure exit code") };
    let success = |ok: TokenStream2| {
        quote! {
            if __code == ::std::process::ExitCode::SUCCESS {
                #ok
            } else {
                #fail
            }
        }
    };
    match entry_ret {
        ReturnType::Default => success(quote! {}),
        ReturnType::Type(_, ty) => {
            if is_unit_type(ty) {
                success(quote! {})
            } else if is_exit_code_type(ty) {
                quote! { __code }
            } else if is_never_type(ty) {
                quote! { match __code { _ => panic!("unreachable: never-returning body") } }
            } else if let Some(ok_ty) = result_ok_type(ty) {
                let ok_value = if is_unit_type(ok_ty) {
                    quote! { () }
                } else if is_exit_code_type(ok_ty) {
                    quote! { ::std::process::ExitCode::SUCCESS }
                } else {
                    quote! { panic!("unsupported Ok type") }
                };
                success(quote! { ::std::result::Result::Ok(#ok_value) })
            } else {
                quote! { __code }
            }
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
    // A soft-fail still passes: a sync entry keeps its `ExitCode::SUCCESS`
    // (its body is a `wrap_with_timeout` block returning `ExitCode`), while an
    // async entry stays `()`.
    let soft_fail_ok = if is_async {
        quote! { () }
    } else {
        quote! { ::std::process::ExitCode::SUCCESS }
    };
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
            #soft_fail_ok
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
                match __result {
                    Ok(__code) => __code,
                    Err(__panic) => {
                        #handle_panic
                    }
                }
            }
        }
    }
}

/// Combine clock measurement, hang budget and optional soft-fail wrapping.
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
    use syn::{Ident, ReturnType, Stmt, parse_quote};

    use super::{exit_code_to_ret, make_clock_measurement_stmt, wrap_with_timeout};

    #[test]
    fn exit_code_to_ret_reconstructs_the_declared_return_type() {
        let result = exit_code_to_ret(&syn::parse_quote!(-> syn::Result<()>)).to_string();
        assert!(result.contains("Result :: Ok"), "{result}");
        assert!(result.contains("panic !"), "{result}");

        let unit = exit_code_to_ret(&ReturnType::Default).to_string();
        assert!(!unit.contains("Result :: Ok"), "{unit}");
        assert!(unit.contains("panic !"), "{unit}");

        let exit_code =
            exit_code_to_ret(&syn::parse_quote!(-> ::std::process::ExitCode)).to_string();
        assert!(!exit_code.contains("Result :: Ok"), "{exit_code}");
        assert!(!exit_code.contains("panic !"), "{exit_code}");
    }

    #[test]
    fn clock_measurement_is_emitted_for_every_target() -> syn::Result<()> {
        let name: Ident = parse_quote!(clock_control);
        let statement: Stmt =
            syn::parse2(make_clock_measurement_stmt(&TestArgs::default(), &name))?;
        let Stmt::Local(binding) = statement else {
            panic!("measurement guard must live in the test body scope");
        };
        assert!(binding.attrs.is_empty(), "every target records its body");
        Ok(())
    }

    #[test]
    fn default_timeout_native_body_installs_prekill_guard() {
        let name: Ident = parse_quote!(stress_case);

        let expanded = wrap_with_timeout(
            &quote!({}),
            &TestArgs::default(),
            false,
            &name,
            &ReturnType::Default,
            &ReturnType::Default,
        )
        .to_string();

        assert!(expanded.contains("PreKillGuard"));
        assert!(expanded.contains("stress_case"));
    }

    #[test]
    fn sync_timeout_records_durable_evidence_before_panicking() {
        let name: Ident = parse_quote!(stress_case);
        let mut args = TestArgs::default();
        args.set_timeout(Some(parse_quote!(::std::time::Duration::from_secs(5))));

        let expanded = wrap_with_timeout(
            &quote!({}),
            &args,
            false,
            &name,
            &ReturnType::Default,
            &ReturnType::Default,
        )
        .to_string();

        assert!(expanded.contains("record_test_hang"));
        assert!(expanded.contains("sync-timeout"));
        assert!(expanded.contains("timed out after"));
    }

    #[test]
    fn async_timeout_records_durable_evidence_before_panicking() {
        let name: Ident = parse_quote!(stress_case);
        let mut args = TestArgs::default();
        args.set_timeout(Some(parse_quote!(::std::time::Duration::from_secs(5))));

        let expanded = wrap_with_timeout(
            &quote!({}),
            &args,
            true,
            &name,
            &ReturnType::Default,
            &ReturnType::Default,
        )
        .to_string();

        assert!(expanded.contains("record_test_hang"));
        assert!(expanded.contains("wall-timeout"));
        assert!(expanded.contains("timed out after"));
    }
}

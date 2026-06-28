use proc_macro::TokenStream;
use quote::quote;
use syn::{ItemFn, parse_macro_input};

use super::parse::WatchdogArgs;

pub(crate) fn expand(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr as WatchdogArgs);
    let input = parse_macro_input!(item as ItemFn);

    let fn_name_str = input.sig.ident.to_string();

    let name_expr = args.name.as_ref().map_or_else(
        || quote! { concat!(module_path!(), "::", #fn_name_str) },
        |lit| quote! { #lit },
    );

    let timeout_expr = args.timeout.as_ref().map_or_else(
        || quote! { ::kithara_test_utils::hang::default_timeout() },
        |expr| quote! { #expr },
    );

    let ctx_type = args.ctx.as_ref().map_or_else(
        || quote! { ::kithara_test_utils::hang::NoContext },
        |ty| quote! { #ty },
    );

    let dump_dir_setup = args.dump_dir.as_ref().map(|expr| {
        quote! {
            __hang_detector = __hang_detector.with_dump_dir(
                ::std::path::PathBuf::from(#expr)
            );
        }
    });

    let attrs = &input.attrs;
    let vis = &input.vis;
    let sig = &input.sig;
    let stmts = &input.block.stmts;

    let output = quote! {
        #(#attrs)*
        #vis #sig {
            let mut __hang_detector: ::kithara_test_utils::hang::HangDetector<#ctx_type> =
                ::kithara_test_utils::hang::HangDetector::new(
                    #name_expr,
                    #timeout_expr,
                );
            #dump_dir_setup
            // `file!()`/`line!()` expand at the `hang_tick!`/`hang_reset!`
            // invocation site (built-in span behaviour through `macro_rules!`),
            // so a fired watchdog reports the exact spinning/last-progress line
            // rather than just the watched function.
            #[allow(unused_macros)]
            macro_rules! hang_tick {
                () => { __hang_detector.tick_from(file!(), line!()); };
                // `$ctx` is wrapped in a closure so the no-`hang` (release)
                // detector drops it uncalled — the app-context collection is
                // provably never executed in release, not merely optimized away.
                ($ctx:expr) => { __hang_detector.tick_with_from(|| $ctx, file!(), line!()); };
            }
            #[allow(unused_macros)]
            macro_rules! hang_reset {
                () => { __hang_detector.reset_from(file!(), line!()); };
                ($ctx:expr) => { __hang_detector.reset_with_from(|| $ctx, file!(), line!()); };
            }
            // Event-driven wait bounded by the liveness budget. `$wait_for` is a
            // `FnOnce(Duration)` that parks the current thread for at most that
            // long (woken early by its event): progress wakes it before the
            // deadline; a genuine stall releases it at the deadline so `tick()`
            // fires. Replaces the `hang_tick!() + small park_timeout` busy-poll.
            #[allow(unused_macros)]
            macro_rules! hang_park {
                ($wait_for:expr) => {{
                    ($wait_for)(__hang_detector.remaining());
                    __hang_detector.tick_from(file!(), line!());
                }};
                ($wait_for:expr, $ctx:expr) => {{
                    ($wait_for)(__hang_detector.remaining());
                    __hang_detector.tick_with_from(|| $ctx, file!(), line!());
                }};
            }
            #(#stmts)*
        }
    };

    output.into()
}

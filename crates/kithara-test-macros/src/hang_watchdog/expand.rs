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
            #[allow(unused_macros)]
            macro_rules! hang_tick {
                () => { __hang_detector.tick(); };
                ($ctx:expr) => { __hang_detector.tick_with($ctx); };
            }
            #[allow(unused_macros)]
            macro_rules! hang_reset {
                () => { __hang_detector.reset(); };
            }
            #(#stmts)*
        }
    };

    output.into()
}

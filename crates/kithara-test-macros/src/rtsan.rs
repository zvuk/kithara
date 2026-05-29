use proc_macro::TokenStream;
use quote::quote;
use syn::{ItemFn, parse_macro_input};

/// Expand `#[kithara::rtsan_forbid_blocking]`.
///
/// Forbids blocking in a function (and everything it calls) by marking it a
/// `RealtimeSanitizer`-checked entry point: emits
/// `#[cfg_attr(rtsan, sanitize(realtime = "nonblocking"))]`. Off `rtsan` the
/// function is byte-identical to the original.
pub(crate) fn expand_forbid_blocking(item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemFn);
    quote! {
        #[cfg_attr(rtsan, sanitize(realtime = "nonblocking"))]
        #input
    }
    .into()
}

/// Expand `#[kithara::rtsan_allow_blocking]`.
///
/// Allows blocking inside an otherwise-`forbid_blocking` context by wrapping
/// the function body in a [`kithara_test_utils::rtsan::permit`] guard, which
/// suspends `RealtimeSanitizer`'s blocking-checks for the whole call (reentrant:
/// nested guards only toggle the runtime at the outermost level). Used to carve
/// a genuinely-unavoidable blocking call (decode, deferred free, idle park, …)
/// out of the checked path. Off `rtsan` the guard is a zero-cost ZST and the
/// function is byte-identical to the original.
pub(crate) fn expand_allow_blocking(item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemFn);
    let attrs = &input.attrs;
    let vis = &input.vis;
    let sig = &input.sig;
    let block = &input.block;
    quote! {
        #(#attrs)*
        #vis #sig {
            let __rtsan_permit = ::kithara_test_utils::rtsan::permit();
            #block
        }
    }
    .into()
}

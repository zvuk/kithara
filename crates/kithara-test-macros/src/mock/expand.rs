use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;

pub(crate) fn expand(args: TokenStream, item: TokenStream) -> TokenStream {
    let args2: TokenStream2 = args.into();
    let item2: TokenStream2 = item.into();

    quote! {
        #[cfg_attr(
            any(test, feature = "mock"),
            allow(
                clippy::ignored_unit_patterns,
                clippy::allow_attributes,
                clippy::needless_lifetimes,
                clippy::redundant_pub_crate,
                clippy::needless_pass_by_value,
                clippy::redundant_closure_for_method_calls,
                clippy::too_many_arguments,
                clippy::elidable_lifetime_names,
                dead_code
            )
        )]
        #[cfg_attr(
            any(test, feature = "mock"),
            ::unimock::unimock(#args2)
        )]
        #item2
    }
    .into()
}

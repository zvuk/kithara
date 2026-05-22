use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::quote;
use syn::{Ident, ItemTrait, parse2};

fn to_snake_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for (i, ch) in s.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

/// Expand `#[kithara::mock(...)]` into a `cfg`-gated module that contains
/// `#[unimock]` + the original trait, with a single `pub use` re-export of
/// every item the module produces.
///
/// The module wrap is the load-bearing piece: `unimock` emits the mock
/// API and the `impl Trait for Unimock` block as siblings to the trait,
/// at the same scope as the input. Block-level inner attributes (`#![...]`)
/// inside the module scope DO propagate to those siblings, while the
/// outer-attribute form `#[cfg_attr(... allow(...))]` attached to the
/// trait does NOT. Lints triggered solely by unimock-generated code
/// (e.g. `clippy::semicolon_if_nothing_returned` on `-> ()` mock bodies)
/// would otherwise leak out through the sibling impl and force callers
/// to suppress them at the use-site.
pub(crate) fn expand(args: TokenStream, item: TokenStream) -> TokenStream {
    let args2: TokenStream2 = args.into();
    let item2: TokenStream2 = item.into();

    let trait_name = parse2::<ItemTrait>(item2.clone()).map_or_else(
        |_| Ident::new("__kithara_mock_trait", Span::call_site()),
        |t| t.ident,
    );
    let mod_ident = Ident::new(
        &format!("__kithara_mock_{}", to_snake_case(&trait_name.to_string())),
        trait_name.span(),
    );

    quote! {
        #[cfg(any(test, feature = "mock"))]
        #[doc(hidden)]
        mod #mod_ident {
            #![allow(
                clippy::ignored_unit_patterns,
                clippy::allow_attributes,
                clippy::needless_lifetimes,
                clippy::redundant_pub_crate,
                clippy::needless_pass_by_value,
                clippy::redundant_closure_for_method_calls,
                clippy::semicolon_if_nothing_returned,
                clippy::too_many_arguments,
                clippy::elidable_lifetime_names,
                dead_code
            )]
            use super::*;
            #[::unimock::unimock(#args2)]
            #item2
        }

        #[cfg(any(test, feature = "mock"))]
        pub use #mod_ident::*;

        #[cfg(not(any(test, feature = "mock")))]
        #item2
    }
    .into()
}

use kithara_test_args::{TestArgs, TestFlags};
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{Attribute, Ident};

use super::{
    expand_async::emit_async_timeout_test,
    fixture_args::Preamble,
    shared::{
        finalize_body, make_ambient_stmt, make_serial_attr, make_test_setup,
        make_wasm_serial_guard, wrap_with_model, wrap_with_sync_participation, wrap_with_timeout,
    },
};

#[expect(clippy::too_many_arguments)]
pub(crate) fn emit_one_test(
    fn_name: &Ident,
    vis: &syn::Visibility,
    ret_type: &syn::ReturnType,
    remaining_attrs: &[&Attribute],
    test_attrs: &TokenStream2,
    is_async: bool,
    preamble: &Preamble,
    body_stmts: &[syn::Stmt],
    args: &TestArgs,
) -> TokenStream2 {
    let test_setup = make_test_setup(args, remaining_attrs);
    let ambient = make_ambient_stmt(args);
    // Async-native emissions install mode per poll; wasm and sync emissions
    // hold both ambient and active scopes in the body.
    let full_plain = preamble.with_body(body_stmts);
    let scoped = preamble.scoped(false);
    let preamble = &preamble.body;
    let body = quote! { #(#body_stmts)* };
    let held_body = if is_async {
        body
    } else {
        wrap_with_sync_participation(&body, args)
    };
    let full_held = quote! { #test_setup #scoped #preamble #ambient #held_body };
    let serial_attr = make_serial_attr(args);
    let wasm_serial_guard = make_wasm_serial_guard(args);

    if is_async {
        let mut output = emit_async_timeout_test(
            fn_name,
            vis,
            ret_type,
            remaining_attrs,
            &full_plain,
            args,
            &serial_attr,
        );
        let wasm_timeout = wrap_with_timeout(&full_held, args, true, fn_name);
        let wasm_wrapped = finalize_body(&wasm_timeout, args, fn_name, true);
        output.extend(quote! {
            #(#remaining_attrs)*
            #[cfg(target_arch = "wasm32")]
            #[wasm_bindgen_test::wasm_bindgen_test]
            #vis async fn #fn_name() #ret_type {
                #wasm_serial_guard
                #wasm_wrapped
            }
        });
        return output;
    }

    let modeled = wrap_with_model(&full_held, args, fn_name);
    let with_timeout = wrap_with_timeout(&modeled, args, false, fn_name);
    let wrapped = finalize_body(&with_timeout, args, fn_name, false);

    if args.flags().contains(TestFlags::SERIAL) {
        return quote! {
            #(#remaining_attrs)*
            #[cfg(not(target_arch = "wasm32"))]
            #serial_attr
            #[test]
            #vis fn #fn_name() #ret_type #wrapped

            #(#remaining_attrs)*
            #[cfg(target_arch = "wasm32")]
            #[wasm_bindgen_test::wasm_bindgen_test]
            #vis async fn #fn_name() #ret_type {
                #wasm_serial_guard
                #wrapped
            }
        };
    }

    quote! {
        #(#remaining_attrs)*
        #serial_attr
        #test_attrs
        #vis fn #fn_name() #ret_type #wrapped
    }
}

/// `full_plain` feeds the async branches (per-poll `with_ambient` is the sole
/// ambient holder there); `full_held` (body-head `ambient_scope`) feeds the
/// sync branch.
pub(crate) fn emit_native_only_one(
    ctx: &super::entry::GenCtx<'_>,
    name: &Ident,
    full_plain: &Preamble,
    full_held: &TokenStream2,
    serial_attr: &TokenStream2,
    native_is_async: bool,
) -> TokenStream2 {
    let remaining_attrs = ctx.remaining_attrs;
    let vis = ctx.vis;
    let ret_type = ctx.ret_type;
    let args = ctx.args;

    if native_is_async {
        return emit_async_timeout_test(
            name,
            vis,
            ret_type,
            remaining_attrs,
            full_plain,
            args,
            serial_attr,
        );
    }
    let modeled = wrap_with_model(full_held, args, name);
    let with_timeout = wrap_with_timeout(&modeled, args, false, name);
    let wrapped = finalize_body(&with_timeout, args, name, false);
    quote! {
        #(#remaining_attrs)*
        #serial_attr
        #[cfg(not(target_arch = "wasm32"))]
        #[test]
        #vis fn #name() #ret_type #wrapped
    }
}

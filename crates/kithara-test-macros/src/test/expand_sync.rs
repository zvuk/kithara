use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{Attribute, Ident};

use super::{
    expand_async::{emit_async_runtime_test, emit_async_timeout_test},
    parse::TestArgs,
    shared::{finalize_body, make_serial_attr, make_tracing_init, wrap_with_timeout},
};

#[expect(clippy::too_many_arguments)]
pub(crate) fn emit_one_test(
    fn_name: &Ident,
    vis: &syn::Visibility,
    ret_type: &syn::ReturnType,
    remaining_attrs: &[&Attribute],
    test_attrs: &TokenStream2,
    is_async: bool,
    preamble: &TokenStream2,
    body_stmts: &[syn::Stmt],
    args: &TestArgs,
) -> TokenStream2 {
    let tracing_init = make_tracing_init(args);
    let full = quote! { #tracing_init #preamble #(#body_stmts)* };
    let serial_attr = make_serial_attr(args);

    if is_async && args.timeout.is_some() {
        let mut output = emit_async_timeout_test(
            fn_name,
            vis,
            ret_type,
            remaining_attrs,
            &full,
            args,
            &serial_attr,
        );
        let wasm_timeout = wrap_with_timeout(&full, &args.timeout, true, fn_name);
        let wasm_wrapped = finalize_body(&wasm_timeout, args, fn_name, true);
        output.extend(quote! {
            #(#remaining_attrs)*
            #[cfg(target_arch = "wasm32")]
            #[wasm_bindgen_test::wasm_bindgen_test]
            #vis async fn #fn_name() #ret_type #wasm_wrapped
        });
        return output;
    }

    if is_async {
        let mut output = emit_async_runtime_test(
            fn_name,
            vis,
            ret_type,
            remaining_attrs,
            &full,
            args,
            &serial_attr,
        );
        let braced = quote! { { #full } };
        let wasm_wrapped = finalize_body(&braced, args, fn_name, true);
        output.extend(quote! {
            #(#remaining_attrs)*
            #[cfg(target_arch = "wasm32")]
            #[wasm_bindgen_test::wasm_bindgen_test]
            #vis async fn #fn_name() #ret_type #wasm_wrapped
        });
        return output;
    }

    let with_timeout = wrap_with_timeout(&full, &args.timeout, false, fn_name);
    let wrapped = finalize_body(&with_timeout, args, fn_name, false);

    quote! {
        #(#remaining_attrs)*
        #serial_attr
        #test_attrs
        #vis fn #fn_name() #ret_type #wrapped
    }
}

pub(crate) fn emit_native_only_one(
    ctx: &super::entry::GenCtx<'_>,
    name: &Ident,
    full: &TokenStream2,
    serial_attr: &TokenStream2,
    native_is_async: bool,
) -> TokenStream2 {
    let remaining_attrs = ctx.remaining_attrs;
    let vis = ctx.vis;
    let ret_type = ctx.ret_type;
    let args = ctx.args;

    if native_is_async && args.timeout.is_some() {
        return emit_async_timeout_test(
            name,
            vis,
            ret_type,
            remaining_attrs,
            full,
            args,
            serial_attr,
        );
    }
    if native_is_async {
        return emit_async_runtime_test(
            name,
            vis,
            ret_type,
            remaining_attrs,
            full,
            args,
            serial_attr,
        );
    }
    let with_timeout = wrap_with_timeout(full, &args.timeout, false, name);
    let wrapped = finalize_body(&with_timeout, args, name, false);
    quote! {
        #(#remaining_attrs)*
        #serial_attr
        #[cfg(not(target_arch = "wasm32"))]
        #[test]
        #vis fn #name() #ret_type #wrapped
    }
}

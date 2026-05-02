//! Proc-macro crate for WASM player exports.
//!
//! Provides `#[wasm_export]` — an attribute macro for methods inside an
//! `impl Player` block. For each annotated method it generates a
//! `#[wasm_bindgen] pub fn player_<name>(...)` free function that delegates
//! to `player().<name>(...)`.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{
    FnArg, Ident, ImplItem, ImplItemFn, ItemFn, ItemImpl, LitStr, Pat, ReturnType, Type,
    parse_macro_input,
};

/// Place on an `impl Player` block. Methods marked `#[export]` get
/// corresponding `#[wasm_bindgen]` free functions generated automatically.
///
/// The accessor function must be named `{lowercase_struct}` and return
/// `&'static StructName`.
///
/// ```ignore
/// #[wasm_export]
/// impl Player {
///     // Not exported — no #[export] attribute.
///     fn send_cmd(&self, cmd: WorkerCmd) -> Result<(), JsValue> { ... }
///
///     #[export]
///     fn play(&self) {
///         let _ = self.send_cmd(WorkerCmd::Play);
///     }
///
///     #[export]
///     async fn select_track(&self, url: String) -> Result<(), JsValue> { ... }
/// }
/// ```
#[proc_macro_attribute]
pub fn wasm_export(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemImpl);
    let accessor_name = impl_accessor_name(&input);

    let mut cleaned_impl = input.clone();
    cleaned_impl.items = Vec::new();
    let mut free_fns = Vec::new();

    for item in &input.items {
        let ImplItem::Fn(method) = item else {
            cleaned_impl.items.push(item.clone());
            continue;
        };

        let has_export = method.attrs.iter().any(|a| a.path().is_ident("export"));

        let mut clean_method = method.clone();
        clean_method.attrs.retain(|a| !a.path().is_ident("export"));
        cleaned_impl.items.push(ImplItem::Fn(clean_method));

        if has_export {
            free_fns.push(build_free_fn(&accessor_name, method));
        }
    }

    let output = quote! {
        #cleaned_impl
        #(#free_fns)*
    };

    output.into()
}

fn impl_accessor_name(input: &ItemImpl) -> Ident {
    let struct_name = match input.self_ty.as_ref() {
        Type::Path(tp) => tp
            .path
            .segments
            .last()
            .expect("empty impl path")
            .ident
            .clone(),
        _ => panic!("wasm_export: expected a named type in impl block"),
    };
    format_ident!("{}", struct_name.to_string().to_lowercase())
}

fn build_free_fn(accessor_name: &Ident, method: &ImplItemFn) -> TokenStream2 {
    let method_name = &method.sig.ident;
    let free_fn_name = format_ident!("{}_{}", accessor_name, method_name);
    let is_async = method.sig.asyncness.is_some();

    let mut param_defs = Vec::new();
    let mut param_names = Vec::new();
    for arg in method.sig.inputs.iter().skip(1) {
        if let FnArg::Typed(pat_type) = arg {
            param_defs.push(quote! { #pat_type });
            if let Pat::Ident(pat_ident) = pat_type.pat.as_ref() {
                param_names.push(pat_ident.ident.clone());
            }
        }
    }

    let ret = match &method.sig.output {
        ReturnType::Default => quote! {},
        ReturnType::Type(_, ty) => quote! { -> #ty },
    };

    let call = if is_async {
        quote! { #accessor_name().#method_name(#(#param_names),*).await }
    } else {
        quote! { #accessor_name().#method_name(#(#param_names),*) }
    };

    let async_token = if is_async {
        quote! { async }
    } else {
        quote! {}
    };

    quote! {
        #[wasm_bindgen]
        pub #async_token fn #free_fn_name(#(#param_defs),*) #ret {
            #call
        }
    }
}

/// Inject a worker-thread assertion at function entry.
///
/// Intended for wasm worker entry points that must never run on the browser
/// main thread.
#[proc_macro_attribute]
pub fn assert_not_main_thread(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let mut input = parse_macro_input!(item as ItemFn);
    let fn_name = LitStr::new(&input.sig.ident.to_string(), input.sig.ident.span());
    let guard = syn::parse_quote! {
        kithara_platform::thread::assert_not_main_thread(concat!(module_path!(), "::", #fn_name));
    };
    input.block.stmts.insert(0, guard);
    quote!(#input).into()
}

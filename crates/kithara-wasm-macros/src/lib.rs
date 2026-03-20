//! Proc-macro crate for WASM player exports.
//!
//! Provides `#[wasm_export]` — an attribute macro for methods inside an
//! `impl Player` block. For each annotated method it generates a
//! `#[wasm_bindgen] pub fn player_<name>(...)` free function that delegates
//! to `player().<name>(...)`.

use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{FnArg, ImplItem, ItemFn, ItemImpl, LitStr, Pat, ReturnType, Type, parse_macro_input};

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
#[expect(
    clippy::missing_panics_doc,
    reason = "proc_macro attribute, panics are compile errors"
)]
pub fn wasm_export(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemImpl);
    let mut free_fns = Vec::new();

    // Extract struct name from `impl Foo { ... }`.
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
    let accessor_name = format_ident!("{}", struct_name.to_string().to_lowercase());

    // Walk methods, collect those marked #[export].
    let mut cleaned_impl = input.clone();
    cleaned_impl.items = Vec::new();

    for item in &input.items {
        let ImplItem::Fn(method) = item else {
            cleaned_impl.items.push(item.clone());
            continue;
        };

        // Check for #[export] attribute.
        let has_export = method.attrs.iter().any(|a| a.path().is_ident("export"));

        // Push method without #[export] into the impl block.
        let mut clean_method = method.clone();
        clean_method.attrs.retain(|a| !a.path().is_ident("export"));
        cleaned_impl.items.push(ImplItem::Fn(clean_method));

        if !has_export {
            continue;
        }

        let method_name = &method.sig.ident;
        let free_fn_name = format_ident!("{}_{}", accessor_name, method_name);
        let is_async = method.sig.asyncness.is_some();

        // Collect parameters (skip &self).
        let mut param_defs = Vec::new(); // `name: Type`
        let mut param_names = Vec::new(); // `name`
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

        // Direct call — no masking, panics are visible.
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

        free_fns.push(quote! {
            #[wasm_bindgen]
            pub #async_token fn #free_fn_name(#(#param_defs),*) #ret {
                #call
            }
        });
    }

    let output = quote! {
        #cleaned_impl
        #(#free_fns)*
    };

    output.into()
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

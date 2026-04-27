//! Proc-macro crate: `#[hang_watchdog]` attribute for loop progress detection.
//!
//! Wraps a function body with a [`HangDetector`] and injects
//! `hang_tick!()` / `hang_reset!()` helper macros usable inside the body.
//!
//! # Usage
//!
//! ```rust,ignore
//! #[hang_watchdog]
//! fn worker_loop() {
//!     loop {
//!         hang_tick!();
//!         // ... do work ...
//!         hang_reset!();
//!     }
//! }
//!
//! #[hang_watchdog(name = "custom.label")]
//! fn read(&mut self, buf: &mut [f32]) -> usize { /* ... */ }
//!
//! #[hang_watchdog(timeout = timeout)]
//! fn wait_range(&mut self, range: Range<u64>, timeout: Duration) -> Result<WaitOutcome> {
//!     // `timeout` here refers to the function parameter
//!     loop { /* ... */ }
//! }
//!
//! // Typed context: dump a snapshot of `FooCtx` when the watchdog fires.
//! #[hang_watchdog(ctx = FooCtx, dump_dir = "/var/log/kithara")]
//! fn decode(&mut self) {
//!     loop {
//!         hang_tick!(FooCtx { phase: self.phase });
//!         // ...
//!     }
//! }
//! ```

use proc_macro::TokenStream;
use quote::quote;
use syn::{
    Expr, ItemFn, LitStr, Token, Type,
    parse::{Parse, ParseStream},
    parse_macro_input,
};

struct WatchdogArgs {
    name: Option<LitStr>,
    timeout: Option<Expr>,
    ctx: Option<Type>,
    dump_dir: Option<Expr>,
}

impl Parse for WatchdogArgs {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let mut name = None;
        let mut timeout = None;
        let mut ctx = None;
        let mut dump_dir = None;

        while !input.is_empty() {
            let ident: syn::Ident = input.parse()?;
            input.parse::<Token![=]>()?;

            match ident.to_string().as_str() {
                "name" => {
                    name = Some(input.parse::<LitStr>()?);
                }
                "timeout" => {
                    timeout = Some(input.parse::<Expr>()?);
                }
                "ctx" => {
                    ctx = Some(input.parse::<Type>()?);
                }
                "dump_dir" => {
                    dump_dir = Some(input.parse::<Expr>()?);
                }
                other => {
                    return Err(syn::Error::new(
                        ident.span(),
                        format!(
                            "unknown attribute `{other}`, expected `name`, `timeout`, `ctx`, or `dump_dir`"
                        ),
                    ));
                }
            }

            if !input.is_empty() {
                input.parse::<Token![,]>()?;
            }
        }

        Ok(Self {
            name,
            timeout,
            ctx,
            dump_dir,
        })
    }
}

/// Wrap a function with a [`HangDetector`](::kithara_hang_detector::HangDetector).
///
/// Inside the function body, two helper macros are available:
/// - `hang_tick!()` — advance the detector's tick counter (no context update).
/// - `hang_tick!(ctx_expr)` — update the detector's stored context and tick.
///   Requires the `ctx = <Type>` attribute so the detector is monomorphized
///   over the correct context type.
/// - `hang_reset!()` — reset the detector (call when progress is made).
///
/// The detector label defaults to `module_path::fn_name` (e.g.
/// `kithara_audio::pipeline::audio::read`). This gives enough context
/// for stack-trace-like diagnostics without manual annotation.
///
/// # Attributes
///
/// - `name = "label"` — custom detector label (default: auto-generated).
/// - `timeout = <expr>` — hang timeout (default: `default_timeout()`).
/// - `ctx = <Type>` — context payload type; enables `hang_tick!(ctx_expr)`.
///   Without this attribute, the detector is monomorphized over
///   [`NoContext`](::kithara_hang_detector::NoContext) and `hang_tick!(...)`
///   with an argument is a compile error.
/// - `dump_dir = <path|expr>` — explicit dump directory (highest precedence).
///   Accepts both string literals and arbitrary `impl Into<PathBuf>` expressions.
#[proc_macro_attribute]
pub fn hang_watchdog(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr as WatchdogArgs);
    let input = parse_macro_input!(item as ItemFn);

    let fn_name_str = input.sig.ident.to_string();

    let name_expr = args.name.as_ref().map_or_else(
        || quote! { concat!(module_path!(), "::", #fn_name_str) },
        |lit| quote! { #lit },
    );

    let timeout_expr = args.timeout.as_ref().map_or_else(
        || quote! { ::kithara_hang_detector::default_timeout() },
        |expr| quote! { #expr },
    );

    let ctx_type = args.ctx.as_ref().map_or_else(
        || quote! { ::kithara_hang_detector::NoContext },
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
            let mut __hang_detector: ::kithara_hang_detector::HangDetector<#ctx_type> =
                ::kithara_hang_detector::HangDetector::new(
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

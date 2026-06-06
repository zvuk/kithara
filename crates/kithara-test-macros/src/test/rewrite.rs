use syn::{
    Expr, ExprCall, ExprPath,
    visit_mut::{self, VisitMut},
};

/// Lexical body-only rewrite for `#[kithara::test(flash(true))]`: retargets a
/// test body's DIRECT calls to the canonical platform time primitives onto the
/// `flash_virtual_*` variants, which hit the quiescence engine UNCONDITIONALLY.
/// This collapses the BODY's own waits onto virtual time without setting
/// `FLASH_ACTIVE`, so a prod fn the body calls keeps its stateless time reads on
/// REAL (engine-coordinated stateful sync primitives still see the per-test
/// ambient gate). Off the `flash-time` feature the targets alias the real
/// primitives, so the rewrite is behaviour-preserving. Matching is by the LAST
/// TWO path segments — the repo routes all time through `kithara_platform::time`,
/// so the recognised path set is fixed and small.
pub(crate) struct FlashRewrite;

impl VisitMut for FlashRewrite {
    fn visit_expr_call_mut(&mut self, call: &mut ExprCall) {
        if let Expr::Path(p) = &*call.func {
            if let Some(repl) = virtual_path(&p.path) {
                *call.func = Expr::Path(repl);
            }
        }
        // Recurse into args and nested calls (the func above was already mapped).
        visit_mut::visit_expr_call_mut(self, call);
    }
}

/// Map a recognised time-fn path (by its last two segments) to the
/// fully-qualified `flash_virtual_*` path; `None` for anything else.
fn virtual_path(path: &syn::Path) -> Option<ExprPath> {
    let segs: Vec<String> = path.segments.iter().map(|s| s.ident.to_string()).collect();
    let n = segs.len();
    if n < 2 {
        return None;
    }
    let last2 = (segs[n - 2].as_str(), segs[n - 1].as_str());
    let repl: syn::Path = match last2 {
        ("time", "sleep") => {
            syn::parse_quote!(::kithara_test_utils::kithara_platform::time::flash_virtual_sleep)
        }
        ("time", "timeout") => {
            syn::parse_quote!(::kithara_test_utils::kithara_platform::time::flash_virtual_timeout)
        }
        ("Instant", "now") => {
            syn::parse_quote!(::kithara_test_utils::kithara_platform::time::flash_virtual_now)
        }
        ("thread", "park_timeout") => {
            syn::parse_quote!(
                ::kithara_test_utils::kithara_platform::time::flash_virtual_park_timeout
            )
        }
        _ => return None,
    };
    Some(ExprPath {
        attrs: vec![],
        qself: None,
        path: repl,
    })
}

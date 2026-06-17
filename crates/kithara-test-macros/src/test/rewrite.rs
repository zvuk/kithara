use proc_macro2::Span;
use syn::{
    Expr, ExprCall, ExprPath,
    visit_mut::{self, VisitMut},
};

/// Lexical body-only rewrite for `#[kithara::test(flash(true))]`: retargets a
/// test body's DIRECT calls to the canonical platform time primitives onto the
/// `flash::virtual_*` variants, which hit the quiescence engine UNCONDITIONALLY.
/// This collapses the BODY's own waits onto virtual time without setting
/// `FLASH_ACTIVE`, so a prod fn the body calls keeps its stateless time reads on
/// REAL (engine-coordinated stateful sync primitives still see the per-test
/// ambient gate). Off the `flash` feature the targets alias the real
/// primitives, so the rewrite is behaviour-preserving. Matching is by the LAST
/// TWO path segments — the repo routes all time through `kithara_platform::time`,
/// so the recognised path set is fixed and small.
///
/// CONVENTION (load-bearing): a flash test body MUST use a QUALIFIED time path
/// (`time::sleep`, `Instant::now`, `thread::park_timeout`), never a bare
/// single-segment import like `use kithara_platform::time::sleep; sleep(d)`. The
/// matcher keys on the last two segments, so a bare-imported `sleep(...)` is one
/// segment and is NOT rewritten — it stays on REAL time inside a flash body, a
/// mixed-clock hazard that can hang the test (this caused the final C3 failure).
/// Bare single-segment matching is intentionally NOT supported: a broad
/// single-segment rule would false-rewrite unrelated `sleep` / `timeout` / `now`
/// calls on other types. Instead the convention is ENFORCED at expansion time:
/// a bare single-segment `sleep(...)` / `timeout(...)` call in a flash body is
/// collected into [`bare_time_calls`](FlashRewrite::bare_time_calls) and the
/// macro rejects it with a compile error (workspace lints exclude test code,
/// so this guard is the only chokepoint that sees flash test bodies). Do NOT
/// broaden the matcher.
#[derive(Default)]
pub(crate) struct FlashRewrite {
    /// Single-segment calls named like a rewritable time primitive: invisible
    /// to the two-segment matcher, they would stay on the REAL clock inside a
    /// virtual body — the mixed-clock hazard above. The caller turns these
    /// into a compile error.
    pub(crate) bare_time_calls: Vec<(Span, String)>,
}

impl VisitMut for FlashRewrite {
    fn visit_expr_call_mut(&mut self, call: &mut ExprCall) {
        if let Expr::Path(p) = &*call.func {
            if let Some(repl) = virtual_path(&p.path) {
                *call.func = Expr::Path(repl);
            } else if p.qself.is_none()
                && p.path.segments.len() == 1
                && matches!(
                    p.path.segments[0].ident.to_string().as_str(),
                    "sleep" | "timeout"
                )
            {
                let ident = &p.path.segments[0].ident;
                self.bare_time_calls.push((ident.span(), ident.to_string()));
            }
        }
        // Recurse into args and nested calls (the func above was already mapped).
        visit_mut::visit_expr_call_mut(self, call);
    }
}

/// Map a recognised time-fn path (by its last two segments) to the
/// fully-qualified `flash::virtual_*` path; `None` for anything else.
fn virtual_path(path: &syn::Path) -> Option<ExprPath> {
    let segs: Vec<String> = path.segments.iter().map(|s| s.ident.to_string()).collect();
    let n = segs.len();
    if n < 2 {
        return None;
    }
    let last2 = (segs[n - 2].as_str(), segs[n - 1].as_str());
    let repl: syn::Path = match last2 {
        ("time", "sleep") => {
            syn::parse_quote!(::kithara_test_utils::kithara_platform::flash::virtual_sleep)
        }
        ("time", "timeout") => {
            syn::parse_quote!(::kithara_test_utils::kithara_platform::flash::virtual_timeout)
        }
        ("Instant", "now") => {
            syn::parse_quote!(::kithara_test_utils::kithara_platform::flash::virtual_now)
        }
        ("thread", "park_timeout") => {
            syn::parse_quote!(::kithara_test_utils::kithara_platform::flash::virtual_park_timeout)
        }
        _ => return None,
    };
    Some(ExprPath {
        attrs: vec![],
        qself: None,
        path: repl,
    })
}

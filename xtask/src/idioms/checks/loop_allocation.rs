//! Heap-allocating expressions inside loop bodies.
//!
//! Recognised triggers (each runs once per iteration, allocating from
//! the global allocator):
//!
//! - `format!(...)` / `format_args!(...)` macros
//! - `.to_string()` / `.to_owned()` / `.to_vec()` method calls
//! - `Vec::new()` / `String::new()` / `HashMap::new()` /
//!   `HashSet::new()` / `BTreeMap::new()` / `BTreeSet::new()`
//! - `Box::new(...)` (every call allocates one box)
//! - `String::from(...)` / `Vec::from(...)`
//!
//! Containers walked: `for`/`while`/`loop` bodies. Iterator-closure
//! bodies (`.map(|x| ...)`, `.filter(|x| ...)`) are out of scope of
//! this initial check — the closure-aware variant lives behind a
//! follow-up extension.
//!
//! `Vec::with_capacity(N)` with a literal N is allowed automatically
//! (it's the allocate-once / reuse pattern this rule promotes). Any
//! method call that's allocating but legitimate (rare cold paths,
//! single-iteration loops, etc.) can be silenced with
//! `// xtask-lint-ignore: loop_allocation`.

use anyhow::Result;
use syn::{
    Expr, ExprCall, ExprForLoop, ExprLoop, ExprMacro, ExprMethodCall, ExprPath, ExprWhile, Macro,
    Path,
    spanned::Spanned,
    visit::{self, Visit},
};

use super::{Check, Context};
use crate::common::{
    parse::parse_file,
    suppress::Suppressions,
    violation::Violation,
    walker::{compile_globs, matches_any, relative_to, workspace_rs_files_scoped},
};

pub(crate) const ID: &str = "loop_allocation";

const EXPLANATION: &str = "\
Detected a heap-allocating expression inside a loop body that runs once \
per iteration.

Why it matters. `format!()`, `String::new()`, `Vec::new()`, `Box::new(...)`, \
`.to_string()`, `.to_owned()` — каждое allocates from the global allocator. \
Inside a loop that runs N times, this is N allocations, N drops, N free-list \
churn rounds. For audio/render hot paths (process callback, decoder loop, \
HLS scheduler tick) even small per-iteration allocations destroy cache \
locality and add jitter to latency-critical code.

The fix is usually one of:
- Hoist the allocation out of the loop.
- Reuse a buffer (`Vec::clear()` + `extend(...)` or `write!(&mut buf, ...)`).
- Use a pool — `kithara_bufpool::{BytePool, PcmPool}` for byte/PCM buffers.
- Pre-size with `Vec::with_capacity(N)` so growth is amortised.

❌  for sample in chunk.frames() { let label = format!(\"frame-{}\", sample.id); log_debug(&label); }
✅  let mut label = String::new(); for sample in chunk.frames() { label.clear(); write!(&mut label, \"frame-{}\", sample.id).unwrap(); log_debug(&label); }

Suppress with `// xtask-lint-ignore: loop_allocation` when the allocation \
is unavoidable (each iteration produces a distinct owned output that \
escapes the loop) or when the loop is cold and the allocation isn't a \
performance concern (initialization, error formatting). `Vec::with_capacity(N)` \
with literal N is allowed automatically.";

pub(crate) struct LoopAllocation;

impl Check for LoopAllocation {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.loop_allocation;
        let exempt = compile_globs(&cfg.exempt_files);
        let mut violations = Vec::new();
        for path in workspace_rs_files_scoped(ctx.workspace_root, ctx.scope)? {
            let rel_path = relative_to(ctx.workspace_root, &path).to_path_buf();
            let rel = rel_path.to_string_lossy().replace('\\', "/");
            if matches_any(&exempt, std::path::Path::new(&rel)) {
                continue;
            }
            let Ok(source) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(file) = parse_file(&path) else {
                continue;
            };
            let suppress = Suppressions::parse(&source);
            let mut v = LoopVisitor {
                rel: &rel,
                suppress: &suppress,
                inside_loop: 0,
                out: &mut violations,
            };
            v.visit_file(&file);
        }
        violations.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(violations)
    }
}

struct LoopVisitor<'a> {
    rel: &'a str,
    suppress: &'a Suppressions,
    /// Depth of enclosing for/while/loop scopes. Allocation expressions
    /// only flag when this is > 0.
    inside_loop: usize,
    out: &'a mut Vec<Violation>,
}

impl LoopVisitor<'_> {
    fn report(&mut self, span_line: usize, span_col: usize, msg: &'static str) {
        if self.suppress.is_suppressed(span_line, ID) {
            return;
        }
        let key = format!("{}:{}:{}", self.rel, span_line, span_col);
        self.out
            .push(Violation::warn(ID, key, msg).with_explanation(EXPLANATION));
    }
}

impl<'ast> Visit<'ast> for LoopVisitor<'_> {
    fn visit_expr_for_loop(&mut self, e: &'ast ExprForLoop) {
        // Walk the iterator expression *outside* the loop scope (it
        // runs once, before iteration), then descend into the body.
        self.visit_expr(&e.expr);
        self.inside_loop += 1;
        self.visit_block(&e.body);
        self.inside_loop -= 1;
    }

    fn visit_expr_while(&mut self, e: &'ast ExprWhile) {
        self.visit_expr(&e.cond);
        self.inside_loop += 1;
        self.visit_block(&e.body);
        self.inside_loop -= 1;
    }

    fn visit_expr_loop(&mut self, e: &'ast ExprLoop) {
        self.inside_loop += 1;
        self.visit_block(&e.body);
        self.inside_loop -= 1;
    }

    fn visit_expr_macro(&mut self, m: &'ast ExprMacro) {
        if self.inside_loop > 0
            && let Some(msg) = format_macro_message(&m.mac)
        {
            let s = m.span().start();
            self.report(s.line, s.column, msg);
        }
        visit::visit_expr_macro(self, m);
    }

    fn visit_expr_call(&mut self, c: &'ast ExprCall) {
        if self.inside_loop > 0
            && let Some(msg) = call_path_message(c)
        {
            let s = c.span().start();
            self.report(s.line, s.column, msg);
        }
        visit::visit_expr_call(self, c);
    }

    fn visit_expr_method_call(&mut self, mc: &'ast ExprMethodCall) {
        if self.inside_loop > 0
            && let Some(msg) = method_message(mc)
        {
            let s = mc.span().start();
            self.report(s.line, s.column, msg);
        }
        visit::visit_expr_method_call(self, mc);
    }
}

fn format_macro_message(m: &Macro) -> Option<&'static str> {
    if path_ends_with(&m.path, "format") {
        return Some(
            "L1: `format!(...)` in loop — hoist the buffer or use `write!` into a reused `String`",
        );
    }
    if path_ends_with(&m.path, "format_args") {
        return Some("L2: `format_args!(...)` in loop — same as L1, allocates per call");
    }
    None
}

fn call_path_message(c: &ExprCall) -> Option<&'static str> {
    let Expr::Path(ExprPath { path, .. }) = &*c.func else {
        return None;
    };
    // Match on the *trailing two* segments — `Vec::new()` or `std::vec::Vec::new()`.
    let segs: Vec<&syn::Ident> = path.segments.iter().map(|s| &s.ident).collect();
    if segs.len() < 2 {
        return None;
    }
    let last = segs[segs.len() - 1];
    let parent = segs[segs.len() - 2];

    // `Vec::with_capacity(LITERAL)` is allowed.
    if parent == "Vec" && last == "with_capacity" {
        let allowed = c
            .args
            .first()
            .is_some_and(|arg| matches!(arg, Expr::Lit(_)));
        if allowed {
            return None;
        }
    }

    let constructors: &[(&str, &str, &str)] = &[
        (
            "Vec",
            "new",
            "L3: `Vec::new()` in loop — pre-size with `Vec::with_capacity(N)` outside the loop, or reuse via `clear()`",
        ),
        (
            "Vec",
            "with_capacity",
            "L3: `Vec::with_capacity(...)` with non-literal in loop — hoist the allocation",
        ),
        (
            "String",
            "new",
            "L3: `String::new()` in loop — reuse via `clear()`",
        ),
        (
            "String",
            "with_capacity",
            "L3: `String::with_capacity(...)` with non-literal in loop — hoist the allocation",
        ),
        (
            "String",
            "from",
            "L4: `String::from(...)` in loop — allocates per call",
        ),
        (
            "Vec",
            "from",
            "L4: `Vec::from(...)` in loop — allocates per call",
        ),
        (
            "HashMap",
            "new",
            "L3: `HashMap::new()` in loop — hoist or reuse via `clear()`",
        ),
        (
            "HashMap",
            "with_capacity",
            "L3: `HashMap::with_capacity(...)` in loop — hoist",
        ),
        (
            "HashSet",
            "new",
            "L3: `HashSet::new()` in loop — hoist or reuse via `clear()`",
        ),
        (
            "BTreeMap",
            "new",
            "L3: `BTreeMap::new()` in loop — hoist or reuse via `clear()`",
        ),
        (
            "BTreeSet",
            "new",
            "L3: `BTreeSet::new()` in loop — hoist or reuse via `clear()`",
        ),
        (
            "Box",
            "new",
            "L5: `Box::new(...)` in loop — every call allocates a new heap slot",
        ),
    ];
    for (p, l, msg) in constructors {
        if parent == p && last == l {
            return Some(msg);
        }
    }
    None
}

fn method_message(mc: &ExprMethodCall) -> Option<&'static str> {
    let name = mc.method.to_string();
    match name.as_str() {
        "to_string" => Some("L6: `.to_string()` in loop — allocates a `String` per iteration"),
        "to_owned" => Some("L6: `.to_owned()` in loop — allocates per iteration"),
        "to_vec" => Some("L6: `.to_vec()` in loop — allocates a `Vec` per iteration"),
        _ => None,
    }
}

fn path_ends_with(path: &Path, name: &str) -> bool {
    path.segments
        .last()
        .is_some_and(|seg| seg.ident == name && seg.arguments.is_none())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count_in(src: &str) -> usize {
        let file: syn::File = syn::parse_str(src).expect("valid Rust source");
        let suppress = Suppressions::parse(src);
        let mut out = Vec::new();
        let mut v = LoopVisitor {
            rel: "fixture.rs",
            suppress: &suppress,
            inside_loop: 0,
            out: &mut out,
        };
        v.visit_file(&file);
        out.len()
    }

    #[test]
    fn format_in_for_loop_flagged() {
        let n = count_in(r#"fn f() { for i in 0..10 { let s = format!("x-{}", i); drop(s); } }"#);
        assert_eq!(n, 1);
    }

    #[test]
    fn format_outside_loop_not_flagged() {
        let n = count_in(r#"fn f() { let s = format!("x"); drop(s); }"#);
        assert_eq!(n, 0);
    }

    #[test]
    fn vec_new_in_loop_flagged() {
        let n = count_in("fn f() { for _ in 0..10 { let v: Vec<u8> = Vec::new(); drop(v); } }");
        assert_eq!(n, 1);
    }

    #[test]
    fn vec_with_capacity_literal_allowed() {
        let n = count_in(
            "fn f() { for _ in 0..10 { let v: Vec<u8> = Vec::with_capacity(64); drop(v); } }",
        );
        assert_eq!(n, 0);
    }

    #[test]
    fn vec_with_capacity_runtime_flagged() {
        let n = count_in(
            "fn f(n: usize) { for _ in 0..10 { let v: Vec<u8> = Vec::with_capacity(n); drop(v); } }",
        );
        assert_eq!(n, 1);
    }

    #[test]
    fn box_new_in_while_flagged() {
        let n = count_in("fn f() { let mut i = 0; while i < 10 { let _ = Box::new(i); i += 1; } }");
        assert_eq!(n, 1);
    }

    #[test]
    fn to_string_in_loop_flagged() {
        let n = count_in(
            r#"fn f(x: &str) { for _ in 0..10 { let s: String = x.to_string(); drop(s); } }"#,
        );
        assert_eq!(n, 1);
    }

    #[test]
    fn iterator_closure_not_flagged_yet() {
        // This initial check intentionally does not descend into iterator
        // closures; that's covered by a follow-up extension.
        let n = count_in(
            r#"fn f() { let _: Vec<String> = (0..10).map(|i| format!("x-{}", i)).collect(); }"#,
        );
        assert_eq!(n, 0);
    }

    #[test]
    fn suppression_works() {
        let src = r#"fn f() {
    for i in 0..10 {
        // xtask-lint-ignore: loop_allocation
        let s = format!("x-{}", i);
        drop(s);
    }
}"#;
        let n = count_in(src);
        assert_eq!(n, 0);
    }

    #[test]
    fn nested_loops_count_once() {
        let n = count_in(
            r#"fn f() { for _ in 0..10 { for _ in 0..10 { let s = format!("x"); drop(s); } } }"#,
        );
        assert_eq!(n, 1);
    }
}

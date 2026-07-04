use anyhow::Result;
use kithara_xtask_core::common::{
    parse::parse_file,
    violation::Violation,
    walker::{compile_globs, matches_any, relative_to, workspace_rs_files_scoped},
};
use syn::{
    BinOp, Block, Expr, ExprAwait, ExprBinary, ExprBreak, ExprContinue, ExprForLoop, ExprIf,
    ExprMethodCall, ExprReturn, Pat, PatIdent, Stmt,
    spanned::Spanned,
    visit::{self, Visit},
};

use super::{Check, Context};
use crate::idioms::config::AccumulatorLoopsConfig;

pub(crate) const ID: &str = "accumulator_loops";

pub(crate) struct AccumulatorLoops;

impl Check for AccumulatorLoops {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.accumulator_loops;
        let exempt = compile_globs(&cfg.exempt_files);
        let mut violations = Vec::new();
        for path in workspace_rs_files_scoped(ctx.workspace_root, ctx.scope)? {
            let rel_path = relative_to(ctx.workspace_root, &path).to_path_buf();
            let rel = rel_path.to_string_lossy().replace('\\', "/");
            if matches_any(&exempt, std::path::Path::new(&rel)) {
                continue;
            }
            let Ok(file) = parse_file(&path) else {
                continue;
            };
            let mut v = LoopVisitor {
                cfg,
                rel: &rel,
                out: &mut violations,
            };
            v.visit_file(&file);
        }
        violations.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(violations)
    }
}

struct LoopVisitor<'a> {
    cfg: &'a AccumulatorLoopsConfig,
    rel: &'a str,
    out: &'a mut Vec<Violation>,
}

impl<'ast> Visit<'ast> for LoopVisitor<'_> {
    fn visit_expr_for_loop(&mut self, e: &'ast ExprForLoop) {
        if let Some(pattern) = classify_loop(self.cfg, e) {
            let s = e.for_token.span().start();
            let key = format!("{}:{}:{}", self.rel, s.line, s.column);
            self.out.push(Violation::warn(ID, key, pattern.message()));
        }
        visit::visit_expr_for_loop(self, e);
    }
}

#[derive(Debug)]
enum Pattern {
    Push,
    Extend,
    Sum,
    Count,
}

impl Pattern {
    fn message(&self) -> &'static str {
        match self {
            Self::Push => {
                "A1: `for ... { acc.push(...) }` — prefer `xs.iter().map(...).collect::<Vec<_>>()` \
                 (one allocation via `size_hint` instead of progressive `Vec::push` growth)"
            }
            Self::Extend => {
                "A4: `for ... { acc.extend(...) }` — prefer `xs.iter().flat_map(...).collect::<Vec<_>>()`"
            }
            Self::Sum => {
                "A2: `for ... { acc += f(x) }` — prefer `xs.iter().map(f).sum()` \
                 (or `.fold(init, |a, x| a + f(x))` if the initial value is non-zero)"
            }
            Self::Count => {
                "A3: `for ... { if pred(x) { n += 1 } }` — prefer `xs.iter().filter(|x| pred(x)).count()`"
            }
        }
    }

    fn enabled(&self, cfg: &AccumulatorLoopsConfig) -> bool {
        let token = match self {
            Self::Push => "push",
            Self::Extend => "extend",
            Self::Sum => "sum",
            Self::Count => "count",
        };
        cfg.detect.iter().any(|t| t == token)
    }
}

fn classify_loop(cfg: &AccumulatorLoopsConfig, e: &ExprForLoop) -> Option<Pattern> {
    if cfg.ignore_with_break && body_has_jump(&e.body) {
        return None;
    }
    if body_has_await(&e.body) {
        return None;
    }
    let stmts = &e.body.stmts;
    let loop_vars = pat_idents(&e.pat);
    let pat = match stmts.len() {
        1 => classify_single_stmt(&stmts[0], &loop_vars)?,
        _ => return None,
    };
    if !pat.enabled(cfg) {
        return None;
    }
    Some(pat)
}

fn classify_single_stmt(s: &Stmt, loop_vars: &[String]) -> Option<Pattern> {
    let expr = stmt_expr(s)?;
    if let Some(p) = classify_method_call(expr, loop_vars) {
        return Some(p);
    }
    if let Some(p) = classify_compound_assign(expr, loop_vars) {
        return Some(p);
    }
    if let Expr::If(if_expr) = expr {
        return classify_if_count(if_expr, loop_vars);
    }
    None
}

/// Root identifier of an assignment target: peels `*x`, `x.field`,
/// `x[i]`, `(x)` down to the base path ident. Used to tell an external
/// accumulator (`acc += ..`) from in-place mutation of the iterated
/// element (`*sample *= gain`), which is a map/scale, not a reduction.
fn assign_target_is_loop_var(target: &Expr, loop_vars: &[String]) -> bool {
    match target {
        Expr::Unary(u) => assign_target_is_loop_var(&u.expr, loop_vars),
        Expr::Field(f) => assign_target_is_loop_var(&f.base, loop_vars),
        Expr::Index(i) => assign_target_is_loop_var(&i.expr, loop_vars),
        Expr::Paren(p) => assign_target_is_loop_var(&p.expr, loop_vars),
        Expr::Path(p) => p
            .path
            .get_ident()
            .is_some_and(|id| loop_vars.iter().any(|v| id == v)),
        _ => false,
    }
}

fn pat_idents(pat: &Pat) -> Vec<String> {
    struct C(Vec<String>);
    impl<'ast> Visit<'ast> for C {
        fn visit_pat_ident(&mut self, pi: &'ast PatIdent) {
            self.0.push(pi.ident.to_string());
            visit::visit_pat_ident(self, pi);
        }
    }
    let mut c = C(Vec::new());
    c.visit_pat(pat);
    c.0
}

fn stmt_expr(s: &Stmt) -> Option<&Expr> {
    match s {
        Stmt::Expr(e, _) => Some(e),
        _ => None,
    }
}

fn classify_method_call(e: &Expr, loop_vars: &[String]) -> Option<Pattern> {
    let Expr::MethodCall(ExprMethodCall {
        method,
        args,
        receiver,
        ..
    }) = e
    else {
        return None;
    };
    if args.len() != 1 {
        return None;
    }
    // `sample.push(..)` mutates the iterated element, not an external Vec.
    if assign_target_is_loop_var(receiver, loop_vars) {
        return None;
    }
    if method == "push" {
        return Some(Pattern::Push);
    }
    if method == "extend" {
        return Some(Pattern::Extend);
    }
    None
}

fn classify_compound_assign(e: &Expr, loop_vars: &[String]) -> Option<Pattern> {
    let Expr::Binary(ExprBinary { op, left, .. }) = e else {
        return None;
    };
    if !matches!(
        op,
        BinOp::AddAssign(_) | BinOp::SubAssign(_) | BinOp::MulAssign(_) | BinOp::DivAssign(_)
    ) {
        return None;
    }
    // In-place mutation of the iterated element (`*sample *= gain`) is a
    // map/scale, not a reduction into a loop-external accumulator.
    if assign_target_is_loop_var(left, loop_vars) {
        return None;
    }
    Some(Pattern::Sum)
}

fn classify_if_count(if_expr: &ExprIf, loop_vars: &[String]) -> Option<Pattern> {
    if if_expr.else_branch.is_some() {
        return None;
    }
    let stmts = &if_expr.then_branch.stmts;
    if stmts.len() != 1 {
        return None;
    }
    let expr = stmt_expr(&stmts[0])?;
    let Expr::Binary(ExprBinary {
        op, left, right, ..
    }) = expr
    else {
        return None;
    };
    if !matches!(op, BinOp::AddAssign(_)) {
        return None;
    }
    if assign_target_is_loop_var(left, loop_vars) {
        return None;
    }
    if !is_lit_one(right) {
        return None;
    }
    Some(Pattern::Count)
}

fn is_lit_one(e: &Expr) -> bool {
    let Expr::Lit(lit) = e else { return false };
    let syn::Lit::Int(int) = &lit.lit else {
        return false;
    };
    int.base10_digits() == "1"
}

fn body_has_jump(b: &Block) -> bool {
    let mut v = JumpFinder { found: false };
    v.visit_block(b);
    v.found
}

/// The accumulated expression contains an `.await`, so the loop is a
/// *sequential async* accumulation. There is no pure `.map().collect()` /
/// `.map().sum()` equivalent: rewriting it requires `futures::stream`
/// (`stream::iter(..).then(..).collect().await`), which changes the
/// concurrency model and pulls in a dependency. Skip it. Nested closures
/// and nested loops own their own `.await`, so they are not descended
/// into — only an await in this loop's own body counts.
fn body_has_await(b: &Block) -> bool {
    let mut v = AwaitFinder { found: false };
    v.visit_block(b);
    v.found
}

struct AwaitFinder {
    found: bool,
}

impl<'ast> Visit<'ast> for AwaitFinder {
    fn visit_expr_await(&mut self, _: &'ast ExprAwait) {
        self.found = true;
    }

    fn visit_expr_closure(&mut self, _: &'ast syn::ExprClosure) {}
    fn visit_expr_for_loop(&mut self, _: &'ast ExprForLoop) {}
    fn visit_expr_loop(&mut self, _: &'ast syn::ExprLoop) {}
    fn visit_expr_while(&mut self, _: &'ast syn::ExprWhile) {}
}

struct JumpFinder {
    found: bool,
}

impl<'ast> Visit<'ast> for JumpFinder {
    fn visit_expr_break(&mut self, _: &'ast ExprBreak) {
        self.found = true;
    }

    fn visit_expr_continue(&mut self, _: &'ast ExprContinue) {
        self.found = true;
    }

    fn visit_expr_return(&mut self, _: &'ast ExprReturn) {
        self.found = true;
    }

    fn visit_expr_closure(&mut self, _: &'ast syn::ExprClosure) {}
    fn visit_expr_for_loop(&mut self, _: &'ast ExprForLoop) {}
    fn visit_expr_loop(&mut self, _: &'ast syn::ExprLoop) {}
    fn visit_expr_while(&mut self, _: &'ast syn::ExprWhile) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count(src: &str) -> usize {
        let cfg = AccumulatorLoopsConfig::default();
        let file: syn::File = syn::parse_str(src).expect("valid Rust source");
        let mut out = Vec::new();
        let mut v = LoopVisitor {
            cfg: &cfg,
            rel: "fixture.rs",
            out: &mut out,
        };
        v.visit_file(&file);
        out.len()
    }

    #[test]
    fn sync_push_flagged() {
        let src = "fn f(xs: &[i32]) -> Vec<i32> { \
                   let mut acc = Vec::new(); \
                   for x in xs { acc.push(*x); } \
                   acc }";
        assert_eq!(count(src), 1);
    }

    #[test]
    fn sync_sum_flagged() {
        let src = "fn f(xs: &[i32]) -> i32 { \
                   let mut total = 0; \
                   for x in xs { total += *x; } \
                   total }";
        assert_eq!(count(src), 1);
    }

    #[test]
    fn in_place_element_scale_not_flagged() {
        // `*sample *= gain` mutates the iterated element (a map/scale),
        // not a loop-external accumulator — there is no `.sum()`/`.fold()`
        // rewrite, so it must not flag.
        let src = "fn f(samples: &mut [f32], gain: f32) { \
                   for sample in samples { *sample *= gain; } }";
        assert_eq!(count(src), 0);
    }

    #[test]
    fn async_push_not_flagged() {
        let src = "async fn f(n: usize) -> Vec<u64> { \
                   let mut acc = Vec::new(); \
                   for i in 0..n { acc.push(estimate(i).await); } \
                   acc }";
        assert_eq!(count(src), 0);
    }

    #[test]
    fn async_sum_not_flagged() {
        let src = "async fn f(n: usize) -> u64 { \
                   let mut total = 0; \
                   for i in 0..n { total += estimate(i).await; } \
                   total }";
        assert_eq!(count(src), 0);
    }

    #[test]
    fn await_in_nested_closure_does_not_exempt() {
        let src = "fn f(xs: &[i32]) -> Vec<i32> { \
                   let mut acc = Vec::new(); \
                   for x in xs { acc.push(map(*x, |y| async move { y.await })); } \
                   acc }";
        assert_eq!(count(src), 1);
    }
}

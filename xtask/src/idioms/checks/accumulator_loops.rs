//! Imperative accumulator loops are usually clearer (and let rustc emit
//! tighter code) when expressed as iterator chains.
//!
//! Recognised patterns (each as a single-statement loop body, or a tight
//! `if`-shaped form for `count`):
//!
//! - **A1 — push**: `for x in xs { v.push(...) }` →
//!   `xs.iter().map(...).collect::<Vec<_>>()`
//! - **A2 — sum/accumulate**: `for x in xs { s += f(x) }` →
//!   `xs.iter().map(f).sum()` (or `.fold(init, |acc, x| acc + f(x))`)
//! - **A3 — conditional count**: `for x in xs { if pred(x) { n += 1 } }` →
//!   `xs.iter().filter(|x| pred(x)).count()`
//! - **A4 — extend / `flat_map`**: `for x in xs { v.extend(f(x)) }` →
//!   `xs.iter().flat_map(f).collect::<Vec<_>>()`
//!
//! Loops containing `break` / `continue` / `return` are skipped by default —
//! the iterator-chain form would change control-flow semantics. Multi-stmt
//! bodies (other than the A3 `if`-shape) are skipped: detecting whether a
//! `fold` is the right replacement requires more semantic context than the
//! AST exposes.

use anyhow::Result;
use syn::{
    BinOp, Block, Expr, ExprBinary, ExprBreak, ExprContinue, ExprForLoop, ExprIf, ExprMethodCall,
    ExprReturn, Stmt,
    spanned::Spanned,
    visit::{self, Visit},
};

use super::{Check, Context};
use crate::{
    common::{
        parse::parse_file,
        violation::Violation,
        walker::{compile_globs, matches_any, relative_to, workspace_rs_files},
    },
    idioms::config::AccumulatorLoopsConfig,
};

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
        for path in workspace_rs_files(ctx.workspace_root)? {
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
    let stmts = &e.body.stmts;
    let pat = match stmts.len() {
        1 => classify_single_stmt(&stmts[0])?,
        _ => return None,
    };
    if !pat.enabled(cfg) {
        return None;
    }
    Some(pat)
}

fn classify_single_stmt(s: &Stmt) -> Option<Pattern> {
    let expr = stmt_expr(s)?;
    if let Some(p) = classify_method_call(expr) {
        return Some(p);
    }
    if let Some(p) = classify_compound_assign(expr) {
        return Some(p);
    }
    if let Expr::If(if_expr) = expr {
        return classify_if_count(if_expr);
    }
    None
}

fn stmt_expr(s: &Stmt) -> Option<&Expr> {
    match s {
        Stmt::Expr(e, _) => Some(e),
        _ => None,
    }
}

fn classify_method_call(e: &Expr) -> Option<Pattern> {
    let Expr::MethodCall(ExprMethodCall { method, args, .. }) = e else {
        return None;
    };
    if args.len() != 1 {
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

fn classify_compound_assign(e: &Expr) -> Option<Pattern> {
    let Expr::Binary(ExprBinary { op, .. }) = e else {
        return None;
    };
    matches!(
        op,
        BinOp::AddAssign(_) | BinOp::SubAssign(_) | BinOp::MulAssign(_) | BinOp::DivAssign(_)
    )
    .then_some(Pattern::Sum)
}

fn classify_if_count(if_expr: &ExprIf) -> Option<Pattern> {
    if if_expr.else_branch.is_some() {
        return None;
    }
    let stmts = &if_expr.then_branch.stmts;
    if stmts.len() != 1 {
        return None;
    }
    let expr = stmt_expr(&stmts[0])?;
    let Expr::Binary(ExprBinary { op, right, .. }) = expr else {
        return None;
    };
    if !matches!(op, BinOp::AddAssign(_)) {
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

    // Don't descend into nested closures/loops — their internal jumps are scoped
    // to themselves and don't affect the outer for-loop's control flow.
    fn visit_expr_closure(&mut self, _: &'ast syn::ExprClosure) {}
    fn visit_expr_for_loop(&mut self, _: &'ast ExprForLoop) {}
    fn visit_expr_loop(&mut self, _: &'ast syn::ExprLoop) {}
    fn visit_expr_while(&mut self, _: &'ast syn::ExprWhile) {}
}

//! `for x in xs { sum += x.weight; names.push(x.name) }` — one loop, multiple
//! independent accumulators. Hint: split into separate iterator chains, or
//! use a `fold((acc1, acc2), ...)` if the two accumulators are coupled.
//!
//! Detection: a `for`-loop body has ≥2 statements, **every** statement matches
//! an accumulator pattern (push / extend / `+=` / `-=` / `*=` / `/=`), and ≥2
//! distinct accumulator targets are present.

use std::collections::BTreeSet;

use anyhow::Result;
use syn::{
    BinOp, Block, Expr, ExprBinary, ExprForLoop, ExprMethodCall, Stmt,
    spanned::Spanned,
    visit::{self, Visit},
};

use super::{Check, Context};
use crate::{
    common::{
        parse::{canonical_subject, parse_file},
        violation::Violation,
        walker::{compile_globs, matches_any, relative_to, workspace_rs_files_scoped},
    },
    idioms::config::MultiAccumulatorLoopConfig,
};

pub(crate) const ID: &str = "multi_accumulator_loop";

pub(crate) struct MultiAccumulatorLoop;

impl Check for MultiAccumulatorLoop {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.multi_accumulator_loop;
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
    cfg: &'a MultiAccumulatorLoopConfig,
    rel: &'a str,
    out: &'a mut Vec<Violation>,
}

impl<'ast> Visit<'ast> for LoopVisitor<'_> {
    fn visit_expr_for_loop(&mut self, e: &'ast ExprForLoop) {
        if let Some(targets) = classify_loop(self.cfg, e) {
            let s = e.for_token.span().start();
            let key = format!("{}:{}:{}", self.rel, s.line, s.column);
            let summary = targets.iter().cloned().collect::<Vec<_>>().join(", ");
            let msg = format!(
                "M1: for-loop accumulates into {} distinct targets (`{summary}`) — \
                 consider splitting into separate iterator chains, or use \
                 `fold((init1, init2), ...)` if the accumulators are coupled",
                targets.len(),
            );
            self.out.push(Violation::warn(ID, key, msg));
        }
        visit::visit_expr_for_loop(self, e);
    }
}

fn classify_loop(cfg: &MultiAccumulatorLoopConfig, e: &ExprForLoop) -> Option<BTreeSet<String>> {
    if cfg.ignore_with_break && body_has_jump(&e.body) {
        return None;
    }
    let stmts = &e.body.stmts;
    if stmts.len() < 2 {
        return None;
    }
    let mut targets: BTreeSet<String> = BTreeSet::new();
    for s in stmts {
        let target = stmt_accumulator_target(s)?;
        targets.insert(target);
    }
    if targets.len() >= 2 {
        Some(targets)
    } else {
        None
    }
}

fn stmt_accumulator_target(s: &Stmt) -> Option<String> {
    let Stmt::Expr(expr, _) = s else {
        return None;
    };
    if let Expr::MethodCall(ExprMethodCall {
        receiver,
        method,
        args,
        ..
    }) = expr
        && (method == "push" || method == "extend")
        && args.len() == 1
    {
        return canonical_subject(receiver);
    }
    if let Expr::Binary(ExprBinary { left, op, .. }) = expr
        && matches!(
            op,
            BinOp::AddAssign(_) | BinOp::SubAssign(_) | BinOp::MulAssign(_) | BinOp::DivAssign(_)
        )
    {
        return canonical_subject(left);
    }
    None
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
    fn visit_expr_break(&mut self, _: &'ast syn::ExprBreak) {
        self.found = true;
    }
    fn visit_expr_continue(&mut self, _: &'ast syn::ExprContinue) {
        self.found = true;
    }
    fn visit_expr_return(&mut self, _: &'ast syn::ExprReturn) {
        self.found = true;
    }
    fn visit_expr_closure(&mut self, _: &'ast syn::ExprClosure) {}
    fn visit_expr_for_loop(&mut self, _: &'ast ExprForLoop) {}
    fn visit_expr_loop(&mut self, _: &'ast syn::ExprLoop) {}
    fn visit_expr_while(&mut self, _: &'ast syn::ExprWhile) {}
}

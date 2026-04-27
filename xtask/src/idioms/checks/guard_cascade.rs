//! Long sequences of early-return guards inside one block hide a classifier
//! that's better expressed as a `match`, a precondition struct, or a
//! decision-table.
//!
//! Recognised guard forms (each consecutive in the same block):
//! - `if cond { return | break | continue | panic!() | bail!() | … }`
//!   without an `else` branch
//! - `let Pat = expr else { return | break | … };` (let-else)
//!
//! ≥ `warn_streak` consecutive guards (default 4) flag the block. The cascade
//! is reported once at the first guard's position; subsequent guards inside
//! the same streak are absorbed.
//!
//! Why it matters: each guard is a separate prediction site; the optimiser
//! cannot collapse heterogeneous guard predicates into a jump table, and
//! readers must trace each guard before reaching the function's actual logic.

use anyhow::Result;
use syn::{
    Block, Expr, Local, Stmt,
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
    idioms::config::GuardCascadeConfig,
};

pub(crate) const ID: &str = "guard_cascade";

pub(crate) struct GuardCascade;

impl Check for GuardCascade {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.guard_cascade;
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
            let mut v = CascadeVisitor {
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

struct CascadeVisitor<'a> {
    cfg: &'a GuardCascadeConfig,
    rel: &'a str,
    out: &'a mut Vec<Violation>,
}

impl<'ast> Visit<'ast> for CascadeVisitor<'_> {
    fn visit_block(&mut self, b: &'ast Block) {
        scan_block(self.cfg, self.rel, b, self.out);
        visit::visit_block(self, b);
    }
}

fn scan_block(cfg: &GuardCascadeConfig, rel: &str, b: &Block, out: &mut Vec<Violation>) {
    let mut i = 0;
    while i < b.stmts.len() {
        if !is_guard_stmt(cfg, &b.stmts[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < b.stmts.len() && is_guard_stmt(cfg, &b.stmts[i]) {
            i += 1;
        }
        let streak = i - start;
        if streak >= cfg.warn_streak {
            let s = b.stmts[start].span().start();
            let key = format!("{}:{}:{}", rel, s.line, s.column);
            let msg = format!(
                "{streak} consecutive guard statements (early-return ifs / let-else) — \
                 consider grouping into a precondition struct, refactoring to a `match`, \
                 or extracting a `validate(...)` helper that returns the decision"
            );
            out.push(Violation::warn(ID, key, msg));
        }
    }
}

fn is_guard_stmt(cfg: &GuardCascadeConfig, s: &Stmt) -> bool {
    match s {
        Stmt::Expr(Expr::If(if_expr), _) => {
            if_expr.else_branch.is_none() && block_ends_with_terminator(cfg, &if_expr.then_branch)
        }
        Stmt::Local(local) => is_let_else_guard(cfg, local),
        _ => false,
    }
}

fn is_let_else_guard(cfg: &GuardCascadeConfig, local: &Local) -> bool {
    let Some(init) = &local.init else {
        return false;
    };
    let Some((_, diverge)) = &init.diverge else {
        return false;
    };
    is_terminator_expr(cfg, diverge)
}

fn block_ends_with_terminator(cfg: &GuardCascadeConfig, b: &Block) -> bool {
    let Some(last) = b.stmts.last() else {
        return false;
    };
    match last {
        Stmt::Expr(e, _) => is_terminator_expr(cfg, e),
        _ => false,
    }
}

fn is_terminator_expr(cfg: &GuardCascadeConfig, e: &Expr) -> bool {
    match e {
        Expr::Return(_) | Expr::Break(_) | Expr::Continue(_) => true,
        Expr::Macro(m) => m
            .mac
            .path
            .get_ident()
            .is_some_and(|id| cfg.terminator_macros.iter().any(|t| id == t)),
        Expr::Block(b) => block_ends_with_terminator(cfg, &b.block),
        Expr::Unsafe(u) => block_ends_with_terminator(cfg, &u.block),
        _ => false,
    }
}

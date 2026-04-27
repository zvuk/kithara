//! Two or more `for`-loops over the same source within one block — a single
//! pass would replace both with one iterator chain, often with `fold` or
//! `unzip`. Two-pass is sometimes deliberate for readability or
//! borrow-checker reasons; treat the hint as a prompt to consider, not a
//! mechanical conversion.
//!
//! Detection: in any block, group `for x in <source>` loops by canonical
//! source string. ≥2 loops in one group flag the block.
//!
//! Caveat: detection trusts the canonical-source equality at the AST level
//! (`&xs` ≡ `&xs`, but `xs.iter()` and `&xs` look different). False
//! negatives are accepted; false positives are rare.

use std::collections::BTreeMap;

use anyhow::Result;
use syn::{Block, ExprForLoop, Stmt, spanned::Spanned, visit::Visit};

use super::{Check, Context};
use crate::common::{
    parse::{canonical_subject, parse_file},
    violation::Violation,
    walker::{compile_globs, matches_any, relative_to, workspace_rs_files},
};

pub(crate) const ID: &str = "parallel_loops";

pub(crate) struct ParallelLoops;

impl Check for ParallelLoops {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.parallel_loops;
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
            let mut v = BlockVisitor {
                rel: &rel,
                out: &mut violations,
            };
            v.visit_file(&file);
        }
        violations.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(violations)
    }
}

struct BlockVisitor<'a> {
    rel: &'a str,
    out: &'a mut Vec<Violation>,
}

impl<'ast> Visit<'ast> for BlockVisitor<'_> {
    fn visit_block(&mut self, b: &'ast Block) {
        scan_block(self.rel, b, self.out);
        syn::visit::visit_block(self, b);
    }
}

fn scan_block(rel: &str, b: &Block, out: &mut Vec<Violation>) {
    let mut by_source: BTreeMap<String, Vec<&ExprForLoop>> = BTreeMap::new();
    for stmt in &b.stmts {
        let Some(loop_expr) = top_level_for_loop(stmt) else {
            continue;
        };
        let Some(src) = canonical_subject(&loop_expr.expr) else {
            continue;
        };
        by_source.entry(src).or_default().push(loop_expr);
    }
    for (source, loops) in by_source {
        if loops.len() < 2 {
            continue;
        }
        let first = loops[0];
        let s = first.for_token.span().start();
        let key = format!("{}:{}:{}::{source}", rel, s.line, s.column);
        let lines: Vec<String> = loops
            .iter()
            .map(|l| l.for_token.span().start().line.to_string())
            .collect();
        let msg = format!(
            "P1: {} sequential `for` loops over `{source}` in the same block (lines {}) — \
             consider merging into one iterator chain or single fold (one pass instead of {})",
            loops.len(),
            lines.join(", "),
            loops.len(),
        );
        out.push(Violation::warn(ID, key, msg));
    }
}

fn top_level_for_loop(s: &Stmt) -> Option<&ExprForLoop> {
    match s {
        Stmt::Expr(syn::Expr::ForLoop(f), _) => Some(f),
        _ => None,
    }
}

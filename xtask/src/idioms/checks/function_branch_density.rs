use std::path::Path;

use anyhow::Result;
use kithara_xtask_core::common::{
    parse::parse_file,
    violation::Violation,
    walker::{compile_globs, matches_any, relative_to, workspace_rs_files_scoped},
};
use syn::{
    BinOp, ExprBinary, ExprIf, ExprMatch, ImplItem, Item, ItemImpl, Local, Stmt,
    spanned::Spanned,
    visit::{self, Visit},
};

use super::{Check, Context};

pub(crate) const ID: &str = "function_branch_density";

pub(crate) struct FunctionBranchDensity;

impl Check for FunctionBranchDensity {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.function_branch_density;
        let exempt = compile_globs(&cfg.exempt_files);
        let mut violations = Vec::new();
        for path in workspace_rs_files_scoped(ctx.workspace_root, ctx.scope)? {
            let rel_path = relative_to(ctx.workspace_root, &path).to_path_buf();
            let rel = rel_path.to_string_lossy().replace('\\', "/");
            if matches_any(&exempt, Path::new(&rel)) {
                continue;
            }
            let Ok(file) = parse_file(&path) else {
                continue;
            };
            collect_dense_fns(&file.items, &rel, cfg.warn_own, &mut violations);
        }
        violations.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(violations)
    }
}

/// Walk every function / method definition in `items`, attributing branch
/// density to the function that *owns* the branches (no call-graph descent).
/// A thin entry point that merely calls into dense callees is not flagged; the
/// dense callee is, at its own declaration span.
fn collect_dense_fns(items: &[Item], rel: &str, warn_own: usize, out: &mut Vec<Violation>) {
    for item in items {
        match item {
            Item::Fn(item_fn) => {
                flag_if_dense(
                    rel,
                    &item_fn.sig.ident.to_string(),
                    item_fn.span(),
                    &item_fn.block.stmts,
                    warn_own,
                    out,
                );
            }
            Item::Impl(item_impl) => walk_impl(rel, item_impl, warn_own, out),
            Item::Mod(m) => {
                if let Some((_, items)) = &m.content {
                    collect_dense_fns(items, rel, warn_own, out);
                }
            }
            _ => {}
        }
    }
}

fn walk_impl(rel: &str, item: &ItemImpl, warn_own: usize, out: &mut Vec<Violation>) {
    for impl_item in &item.items {
        if let ImplItem::Fn(method) = impl_item {
            flag_if_dense(
                rel,
                &method.sig.ident.to_string(),
                method.span(),
                &method.block.stmts,
                warn_own,
                out,
            );
            for stmt in &method.block.stmts {
                if let Stmt::Item(Item::Mod(m)) = stmt
                    && let Some((_, items)) = &m.content
                {
                    collect_dense_fns(items, rel, warn_own, out);
                }
            }
        }
    }
}

fn flag_if_dense(
    rel: &str,
    name: &str,
    span: proc_macro2::Span,
    body: &[Stmt],
    warn_own: usize,
    out: &mut Vec<Violation>,
) {
    let mut counter = BranchCounter { own_branches: 0 };
    counter.visit_stmts(body);
    if counter.own_branches <= warn_own {
        return;
    }
    let s = span.start();
    let key = format!("{rel}:{}:{}", s.line, s.column);
    let msg = format!(
        "`{name}` has {} branch decisions in its own body (if / match-dispatch / loop / \
         && / || / let-else). A single function this branch-dense stresses the branch \
         predictor and reader. Consider a tag-enum + match dispatch, caching predicate state \
         (compute once when it changes, read once on the hot path), or splitting genuinely \
         independent responsibilities. Extracting the branches into helpers called from here \
         does not reduce the count — the same conditional jumps still execute.",
        counter.own_branches
    );
    out.push(Violation::warn(ID, key, msg));
}

struct BranchCounter {
    own_branches: usize,
}

impl BranchCounter {
    fn visit_stmts(&mut self, stmts: &[Stmt]) {
        for s in stmts {
            self.visit_stmt(s);
        }
    }
}

impl<'ast> Visit<'ast> for BranchCounter {
    fn visit_expr_if(&mut self, node: &'ast ExprIf) {
        self.own_branches += 1;
        visit::visit_expr_if(self, node);
    }

    fn visit_expr_match(&mut self, node: &'ast ExprMatch) {
        // A `match` lowers to a jump table / balanced decision tree, not an
        // `if/else-if` ladder: variant dispatch is one control-flow decision
        // regardless of arm count, so count it as a single branch (like an
        // `if`). Each arm *guard* is a sequential conditional the CPU
        // evaluates in arm order, so it counts +1 — a guard-ladder disguised
        // as a `match` is still measured. Nested control flow in arm bodies
        // and guards is counted by descent.
        if node.arms.len() > 1 {
            self.own_branches += 1;
        }
        self.own_branches += node.arms.iter().filter(|arm| arm.guard.is_some()).count();
        visit::visit_expr_match(self, node);
    }

    fn visit_expr_while(&mut self, node: &'ast syn::ExprWhile) {
        self.own_branches += 1;
        visit::visit_expr_while(self, node);
    }

    fn visit_expr_for_loop(&mut self, node: &'ast syn::ExprForLoop) {
        self.own_branches += 1;
        visit::visit_expr_for_loop(self, node);
    }

    fn visit_expr_loop(&mut self, node: &'ast syn::ExprLoop) {
        self.own_branches += 1;
        visit::visit_expr_loop(self, node);
    }

    fn visit_expr_try(&mut self, node: &'ast syn::ExprTry) {
        // `?` lowers to a discriminant test + cold early-return. LLVM marks
        // the Err/None arm `unlikely` and the branch predictor nails a
        // consistently-not-taken branch at ~zero cost, so error-plumbing
        // does not contribute the hot-path pressure this check measures. It
        // is not counted; real data-dependent control flow inside the
        // fallible expression is still counted by descent.
        visit::visit_expr_try(self, node);
    }

    fn visit_expr_binary(&mut self, node: &'ast ExprBinary) {
        if matches!(node.op, BinOp::And(_) | BinOp::Or(_)) {
            self.own_branches += 1;
        }
        visit::visit_expr_binary(self, node);
    }

    fn visit_local(&mut self, node: &'ast Local) {
        if let Some(init) = &node.init
            && init.diverge.is_some()
        {
            self.own_branches += 1;
        }
        visit::visit_local(self, node);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count_body(body: &str) -> usize {
        let block: syn::Block = syn::parse_str(body).expect("valid Rust block");
        let mut counter = BranchCounter { own_branches: 0 };
        counter.visit_stmts(&block.stmts);
        counter.own_branches
    }

    #[test]
    fn counts_if_and_else_if() {
        let n = count_body(
            r#"{
                if a { 1 } else if b { 2 } else { 3 };
            }"#,
        );
        assert_eq!(n, 2);
    }

    #[test]
    fn guardless_match_dispatch_counts_as_one() {
        // A guardless `match` is a single jump-table dispatch, not an
        // `if/else-if` ladder — arm count must not inflate the score.
        let n = count_body(
            r#"{
                match x {
                    A => 1,
                    B => 2,
                    C => 3,
                };
            }"#,
        );
        assert_eq!(n, 1);
    }

    #[test]
    fn match_arm_guards_each_count_as_a_branch() {
        // Guards are sequential conditionals the CPU evaluates in arm
        // order: 1 dispatch + 2 guards = 3.
        let n = count_body(
            r#"{
                match x {
                    A if a => 1,
                    B if b => 2,
                    _ => 3,
                };
            }"#,
        );
        assert_eq!(n, 3);
    }

    #[test]
    fn counts_let_else_and_logical_ops_but_not_question_mark() {
        // let-else (+1), if (+1), && (+1) count; the `?` is a predictable
        // cold early-return and is not counted.
        let n = count_body(
            r#"{
                let Some(_a) = opt else { return; };
                if x && y { run()?; }
            }"#,
        );
        assert_eq!(n, 3);
    }

    #[test]
    fn question_mark_alone_counts_zero() {
        // A bare `?` is a predictable cold early-return — no hot-path
        // branch-predictor pressure, so it must not score.
        let n = count_body(
            r#"{
                let _v = run()?;
                let _w = step()?;
            }"#,
        );
        assert_eq!(n, 0);
    }

    #[test]
    fn nested_control_flow_is_counted_by_descent() {
        // Branch density is the owner's whole-body count: nested loops and
        // conditionals inside the function body all accrue to this function,
        // since extracting them into helpers would not reduce the jumps.
        let n = count_body(
            r#"{
                for _x in xs {
                    if a { run(); }
                    match k { A => 1, B => 2, _ => 3 };
                }
            }"#,
        );
        // for (1) + if (1) + match dispatch (1) = 3.
        assert_eq!(n, 3);
    }

    #[test]
    fn straight_line_body_scores_zero() {
        // A thin pass-through / data-plumbing body owns no branches even if it
        // calls into dense callees — owner-attribution does not blame it.
        let n = count_body(
            r#"{
                let a = first();
                let b = second(a);
                third(a, b)
            }"#,
        );
        assert_eq!(n, 0);
    }
}

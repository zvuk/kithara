use std::collections::HashSet;

use anyhow::Result;
use syn::{
    Block, Expr, ExprPath, Local, PatIdent, Stmt,
    spanned::Spanned,
    visit::{self, Visit},
};

use super::{Check, Context};
use crate::{
    common::{
        parse::parse_file,
        violation::Violation,
        walker::{compile_globs, matches_any, relative_to, workspace_rs_files_scoped},
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
        for path in workspace_rs_files_scoped(ctx.workspace_root, ctx.scope)? {
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
    out: &'a mut Vec<Violation>,
    rel: &'a str,
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
        if streak >= cfg.warn_streak && !is_dependency_chain(&b.stmts[start..i]) {
            let s = b.stmts[start].span().start();
            let key = format!("{}:{}:{}", rel, s.line, s.column);
            let msg = format!(
                "{streak} consecutive guard statements (early-return ifs / let-else). \
                 Real fixes — heterogeneous (distinct return values): replace with parallel \
                 `let` computations followed by a single `match` over the tuple. \
                 Homogeneous (all guards exit with the same terminator): extract an \
                 Option-returning resolver and call it once. \
                 NOT a fix: extracting guards into a helper returning `Result<_, Reason>` — \
                 it inlines back to the same cascade. \
                 See xtask/src/idioms/checks/guard_cascade.rs module docs for examples."
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
        // `break`/`continue` are loop control, not a guard ladder protecting
        // a happy path — there is no parallel-compute/tuple-match remedy for
        // them, so they must not count as guard terminators.
        Expr::Return(_) => true,
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

/// True when the guards form a binding-dependency chain: some guard's test
/// expression references an identifier bound by an earlier guard (e.g.
/// `let Some(item) = map.get(&id) else {..}` after `let Some(id) = ..`).
/// The only collapse for such a chain is an Option-returning resolver
/// helper — a wrapper that inlines back to the same chain — so the cascade
/// is not independently parallelizable and must not be flagged.
fn is_dependency_chain(guards: &[Stmt]) -> bool {
    let mut bound: HashSet<String> = HashSet::new();
    for stmt in guards {
        if let Some(test) = guard_test_expr(stmt)
            && !collect_idents(test).is_disjoint(&bound)
        {
            return true;
        }
        for id in let_else_bound_idents(stmt) {
            bound.insert(id);
        }
    }
    false
}

fn guard_test_expr(s: &Stmt) -> Option<&Expr> {
    match s {
        Stmt::Expr(Expr::If(if_expr), _) => Some(&if_expr.cond),
        Stmt::Local(local) => local.init.as_ref().map(|init| &*init.expr),
        _ => None,
    }
}

fn let_else_bound_idents(s: &Stmt) -> Vec<String> {
    let Stmt::Local(local) = s else {
        return Vec::new();
    };
    let mut c = PatIdentCollector(Vec::new());
    c.visit_pat(&local.pat);
    c.0
}

fn collect_idents(expr: &Expr) -> HashSet<String> {
    let mut c = PathIdentCollector(HashSet::new());
    c.visit_expr(expr);
    c.0
}

struct PathIdentCollector(HashSet<String>);
impl<'ast> Visit<'ast> for PathIdentCollector {
    fn visit_expr_path(&mut self, ep: &'ast ExprPath) {
        if let Some(id) = ep.path.get_ident() {
            self.0.insert(id.to_string());
        }
        visit::visit_expr_path(self, ep);
    }
}

struct PatIdentCollector(Vec<String>);
impl<'ast> Visit<'ast> for PatIdentCollector {
    fn visit_pat_ident(&mut self, pi: &'ast PatIdent) {
        self.0.push(pi.ident.to_string());
        visit::visit_pat_ident(self, pi);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct V<'a> {
        cfg: &'a GuardCascadeConfig,
        out: &'a mut Vec<Violation>,
    }
    impl<'ast> Visit<'ast> for V<'_> {
        fn visit_block(&mut self, b: &'ast Block) {
            scan_block(self.cfg, "fixture.rs", b, self.out);
            visit::visit_block(self, b);
        }
    }

    fn count_violations(body: &str) -> usize {
        let cfg = GuardCascadeConfig::default();
        let block: Block = syn::parse_str(body).expect("valid Rust block");
        let mut out = Vec::new();
        let mut v = V {
            cfg: &cfg,
            out: &mut out,
        };
        v.visit_block(&block);
        out.len()
    }

    #[test]
    fn cascade_heterogeneous_flagged() {
        let n = count_violations(
            r#"{
                if locked { return 1; }
                if manual { return 2; }
                if cant_switch { return 3; }
                if warming { return 4; }
                let Some(x) = estimate else { return 5; };
                x
            }"#,
        );
        assert_eq!(n, 1, "heterogeneous cascade must be flagged");
    }

    #[test]
    fn dependency_chain_not_flagged() {
        // Each guard extracts from the previous binding (entry → peer →
        // progress). The only collapse is an Option-resolver wrapper, which
        // inlines back to the same chain — so this is not a real cascade.
        let n = count_violations(
            r#"{
                let Some(entry) = peer_entry else { return; };
                if entry.is_locked() { return; }
                let Some(peer) = entry.peer.upgrade() else { return; };
                let Some(progress) = peer.progress() else { return; };
                publish(progress);
            }"#,
        );
        assert_eq!(n, 0, "dependency-chain cascade must NOT be flagged");
    }

    #[test]
    fn loop_control_break_continue_not_flagged() {
        // break/continue are loop flow, not a guard ladder to a happy path.
        let n = count_violations(
            r#"{
                for x in items {
                    let Some(entry) = lookup(x) else { continue; };
                    if !entry.ready() { continue; }
                    if entry.done() { break; }
                    process(entry);
                }
            }"#,
        );
        assert_eq!(n, 0, "loop break/continue control must NOT be flagged");
    }

    #[test]
    fn independent_if_guard_decision_table_still_flagged() {
        // Independent checks (no inter-guard dependency) at the 4+ streak
        // remain a genuine decision-table candidate (parallel-compute +
        // tuple-match).
        let n = count_violations(
            r#"{
                if a == 0 { return 1; }
                if b == 0 { return 2; }
                if c == 0 { return 3; }
                if d == 0 { return 4; }
                5
            }"#,
        );
        assert_eq!(n, 1, "independent 4-guard decision-table must flag");
    }

    #[test]
    fn three_independent_guards_idiomatic_not_flagged() {
        // 3 independent validation guards are idiomatic and below threshold.
        let n = count_violations(
            r#"{
                if a == 0 { return 1; }
                if b == 0 { return 2; }
                if c == 0 { return 3; }
                4
            }"#,
        );
        assert_eq!(n, 0, "3 idiomatic validation guards must NOT flag");
    }

    #[test]
    fn parallel_compute_match_clean() {
        let n = count_violations(
            r#"{
                let locked = compute_locked();
                let manual = compute_manual();
                let cant_switch = compute_cant_switch();
                let warming = compute_warming();
                let estimate = compute_estimate();

                let bps: u64 = match (locked, manual, cant_switch, warming, estimate) {
                    (true, _, _, _, _) => return decision(1),
                    (_, Some(_), _, _, _) => return decision(2),
                    (_, _, true, _, _) => return decision(3),
                    (_, _, _, true, _) => return decision(4),
                    (_, _, _, _, None) => return decision(5),
                    (false, None, false, false, Some(b)) => b,
                };
                bps
            }"#,
        );
        assert_eq!(n, 0, "parallel compute + tuple match must NOT flag");
    }

    #[test]
    fn option_resolver_clean() {
        let n = count_violations(
            r#"{
                let Some(ctx) = self.resolve_ctx(peer_id) else { return; };
                publish(ctx.progress);
            }"#,
        );
        assert_eq!(n, 0, "single Option-resolver let-else must NOT flag");
    }

    #[test]
    fn two_guards_below_threshold_clean() {
        let n = count_violations(
            r#"{
                if a { return 1; }
                if b { return 2; }
                done
            }"#,
        );
        assert_eq!(n, 0, "2 guards under threshold must NOT flag");
    }
}

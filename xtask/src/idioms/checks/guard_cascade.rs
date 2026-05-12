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
    fn cascade_homogeneous_flagged() {
        let n = count_violations(
            r#"{
                let Some(entry) = peer_entry else { return; };
                if entry.is_locked() { return; }
                let Some(peer) = entry.peer.upgrade() else { return; };
                let Some(progress) = peer.progress() else { return; };
                publish(progress);
            }"#,
        );
        assert_eq!(n, 1, "homogeneous cascade must be flagged");
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

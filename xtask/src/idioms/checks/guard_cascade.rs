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
//! # Why it matters
//!
//! Each guard is a separate prediction site; the optimiser cannot collapse
//! heterogeneous guard predicates into a jump table, and readers must trace
//! each guard before reaching the function's actual logic.
//!
//! # What is a fix — heterogeneous cascade (each guard returns a distinct value)
//!
//! Replace the cascade with **parallel compute + a single `match`** over a
//! tuple of the computed values. The match is one statement, not a cascade,
//! and the compiler sees the entire decision in one scope (jump table
//! candidate for discriminant matches, branchless for boolean tuples).
//!
//! ```ignore
//! // before — cascade with distinct AbrReason returns
//! if state.is_locked()        { return decision(.., AbrReason::Locked); }
//! if let Manual(idx) = mode   { return decision(.., AbrReason::ManualOverride); }
//! if !can_switch_now           { return decision(.., AbrReason::MinInterval); }
//! if warming                   { return decision(.., AbrReason::Warmup); }
//! let Some(bps) = estimate_bps else { return decision(.., AbrReason::NoEstimate); };
//!
//! // after — parallel compute, one tuple match site
//! let locked        = state.is_locked();
//! let manual_target = if let Manual(i) = mode { Some(i) } else { None };
//! let cant_switch   = !can_switch_now;
//! let warming       = bytes_dl < warmup_min;
//! let estimate      = view.estimate_bps;
//!
//! let bps = match (locked, manual_target, cant_switch, warming, estimate) {
//!     (true, _, _, _, _)              => return decision(.., AbrReason::Locked),
//!     (_, Some(i), _, _, _)           => return decision(.., AbrReason::ManualOverride),
//!     (_, _, true, _, _)              => return decision(.., AbrReason::MinInterval),
//!     (_, _, _, true, _)              => return decision(.., AbrReason::Warmup),
//!     (_, _, _, _, None)              => return decision(.., AbrReason::NoEstimate),
//!     (false, None, false, false, Some(b)) => b,
//! };
//! ```
//!
//! # What is a fix — homogeneous cascade (every guard exits with the same terminator)
//!
//! Extract an `Option`-returning resolver and call it once. Inside the
//! resolver, `?`-operators chain the missing-context checks; the lint does
//! NOT consider `?` a guard, and the compiler reduces the chain to a single
//! "any check failed → None" branch.
//!
//! ```ignore
//! // before — cascade of homogeneous early-exits
//! let Some(entry) = self.peer_entry(peer_id) else { return; };
//! if entry.state.as_ref().is_some_and(|s| s.is_locked()) { return; }
//! let Some(peer) = entry.peer_weak.upgrade() else { return; };
//! let Some(progress) = peer.progress() else { return; };
//!
//! // after — single resolver call, no cascade in the caller
//! let Some(ctx) = self.resolve_ctx(peer_id) else { return; };
//!
//! fn resolve_ctx(&self, peer_id: PeerId) -> Option<Ctx<'_>> {
//!     let entry = self.peer_entry(peer_id)?;
//!     if entry.state.as_ref().is_some_and(|s| s.is_locked()) { return None; }
//!     let peer = entry.peer_weak.upgrade()?;
//!     let progress = peer.progress()?;
//!     Some(Ctx { entry, peer, progress })
//! }
//! ```
//!
//! # What is NOT a fix
//!
//! Extracting a heterogeneous cascade into a helper that returns
//! `Result<Premise, Reason>` and then `match`ing on it in the caller is a
//! text-level workaround, not an architectural fix. The compiler inlines the
//! helper back to the same cascade — the assembly is identical, the branch
//! prediction problem is unchanged. The only thing that moves is the lint's
//! AST view; the lint stops firing because the cascade is now in another
//! function, not because the cascade is gone.
//!
//! Same goes for closure-wrapping (`(|| { … })()`) — the closure body has
//! the cascade.
//!
//! Real fixes are listed in the two sections above.

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

    fn count_violations(body: &str) -> usize {
        let cfg = GuardCascadeConfig::default();
        let block: Block = syn::parse_str(body).expect("valid Rust block");
        let mut out = Vec::new();
        // Walk the parsed file the same way the production visitor does so
        // that nested blocks are evaluated, not just the top-level.
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
        let mut v = V {
            cfg: &cfg,
            out: &mut out,
        };
        v.visit_block(&block);
        out.len()
    }

    #[test]
    fn cascade_heterogeneous_flagged() {
        // The shape of `AbrState::decide()` before the refactor: 5 consecutive
        // guards each returning a distinct value. Real CPU-prediction problem.
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
        // The shape of `AbrController::check_incoherence()` before extraction:
        // 4 consecutive guards all using `return;`. Cosmetic cascade.
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
        // The refactored `decide()` shape: 5 plain `let` computations followed
        // by ONE tuple-match. Pure `let`s are not guards; the match itself is
        // a single Stmt — not a cascade.
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
        // The refactored homogeneous shape: ONE `let-else` calling a resolver.
        // `?`-chains live inside the resolver and are not detected as guards
        // by this lint, so the resolver body is also clean.
        let n = count_violations(
            r#"{
                let Some(ctx) = self.resolve_ctx(peer_id) else { return; };
                publish(ctx.progress);
            }"#,
        );
        assert_eq!(n, 0, "single Option-resolver let-else must NOT flag");
    }

    #[test]
    fn three_guards_below_threshold_clean() {
        // Sanity: default `warn_streak` is 4. Three guards is not yet a cascade.
        let n = count_violations(
            r#"{
                if a { return 1; }
                if b { return 2; }
                if c { return 3; }
                done
            }"#,
        );
        assert_eq!(n, 0, "3 guards under threshold must NOT flag");
    }
}

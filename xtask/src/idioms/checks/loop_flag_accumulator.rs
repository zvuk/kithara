use std::collections::HashSet;

use anyhow::Result;
use kithara_xtask_core::common::{
    parse::parse_file,
    suppress::Suppressions,
    violation::Violation,
    walker::{compile_globs, matches_any, relative_to, workspace_rs_files_scoped},
};
use syn::{
    BinOp, Block, Expr, ExprAssign, ExprBinary, ExprForLoop, ExprIf, ExprLoop, ExprPath, ExprWhile,
    Lit, Local, Pat, PatIdent, Stmt, visit::Visit,
};

use super::{Check, Context};

pub(crate) const ID: &str = "loop_flag_accumulator";

pub(crate) struct LoopFlagAccumulator;

impl Check for LoopFlagAccumulator {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.loop_flag_accumulator;
        let exempt = compile_globs(&cfg.exempt_files);
        let mut violations = Vec::new();
        for path in workspace_rs_files_scoped(ctx.workspace_root, ctx.scope)? {
            let rel = relative_to(ctx.workspace_root, &path);
            if matches_any(&exempt, rel) {
                continue;
            }
            let Ok(file) = parse_file(&path) else {
                continue;
            };
            let src = std::fs::read_to_string(&path)?;
            let suppress = Suppressions::parse(&src);
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            analyze_file(&rel_str, &file, &suppress, &mut violations);
        }
        violations.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(violations)
    }
}

fn analyze_file(rel: &str, file: &syn::File, sup: &Suppressions, out: &mut Vec<Violation>) {
    struct V<'a> {
        rel: &'a str,
        sup: &'a Suppressions,
        out: &'a mut Vec<Violation>,
    }
    impl<'ast> Visit<'ast> for V<'_> {
        fn visit_item_fn(&mut self, f: &'ast syn::ItemFn) {
            let reset = reset_flags(&f.block);
            scan_block(
                self.rel,
                &f.block,
                &HashSet::new(),
                &reset,
                self.sup,
                self.out,
            );
            syn::visit::visit_item_fn(self, f);
        }
        fn visit_impl_item_fn(&mut self, f: &'ast syn::ImplItemFn) {
            let reset = reset_flags(&f.block);
            scan_block(
                self.rel,
                &f.block,
                &HashSet::new(),
                &reset,
                self.sup,
                self.out,
            );
            syn::visit::visit_impl_item_fn(self, f);
        }
    }
    let mut v = V { rel, sup, out };
    v.visit_file(file);
}

/// Identifiers assigned `false` anywhere in the function body. A flag that
/// is toggled both ways is mutable cross-iteration / dedup state, not a
/// monotone find-first latch reducible to `.any()`, so it must not be
/// flagged.
fn reset_flags(block: &Block) -> HashSet<String> {
    struct Reset {
        names: HashSet<String>,
    }
    impl<'ast> Visit<'ast> for Reset {
        fn visit_expr_assign(&mut self, e: &'ast ExprAssign) {
            if is_false_lit(&e.right)
                && let Some(name) = single_ident(&e.left)
            {
                self.names.insert(name);
            }
            syn::visit::visit_expr_assign(self, e);
        }
        fn visit_expr_binary(&mut self, e: &'ast ExprBinary) {
            if matches!(e.op, BinOp::BitAndAssign(_))
                && is_false_lit(&e.right)
                && let Some(name) = single_ident(&e.left)
            {
                self.names.insert(name);
            }
            syn::visit::visit_expr_binary(self, e);
        }
    }
    let mut r = Reset {
        names: HashSet::new(),
    };
    r.visit_block(block);
    r.names
}

fn scan_block(
    rel: &str,
    block: &Block,
    parent_bools: &HashSet<String>,
    reset: &HashSet<String>,
    sup: &Suppressions,
    out: &mut Vec<Violation>,
) {
    let mut bools: HashSet<String> = parent_bools.clone();
    for stmt in &block.stmts {
        if let Stmt::Local(loc) = stmt
            && let Some(name) = bool_let_name(loc)
        {
            bools.insert(name);
        }
        if let Some(expr) = stmt_expr(stmt) {
            inspect_expr(rel, expr, &bools, reset, sup, out);
        }
    }
}

fn bool_let_name(loc: &Local) -> Option<String> {
    let Pat::Ident(PatIdent { ident, .. }) = &loc.pat else {
        return None;
    };
    let Some(init) = &loc.init else { return None };
    if !matches!(&*init.expr, Expr::Lit(l) if matches!(l.lit, Lit::Bool(_))) {
        return None;
    }
    Some(ident.to_string())
}

fn stmt_expr(stmt: &Stmt) -> Option<&Expr> {
    match stmt {
        Stmt::Expr(e, _) => Some(e),
        Stmt::Local(loc) => loc.init.as_ref().map(|i| &*i.expr),
        _ => None,
    }
}

fn inspect_expr(
    rel: &str,
    expr: &Expr,
    bools: &HashSet<String>,
    reset: &HashSet<String>,
    sup: &Suppressions,
    out: &mut Vec<Violation>,
) {
    match expr {
        Expr::ForLoop(ExprForLoop {
            body, for_token, ..
        }) => {
            check_loop(
                rel,
                body,
                for_token.span.start().line,
                bools,
                reset,
                sup,
                out,
            );
            scan_block(rel, body, bools, reset, sup, out);
        }
        Expr::While(ExprWhile {
            body, while_token, ..
        }) => {
            check_loop(
                rel,
                body,
                while_token.span.start().line,
                bools,
                reset,
                sup,
                out,
            );
            scan_block(rel, body, bools, reset, sup, out);
        }
        Expr::Loop(ExprLoop {
            body, loop_token, ..
        }) => {
            check_loop(
                rel,
                body,
                loop_token.span.start().line,
                bools,
                reset,
                sup,
                out,
            );
            scan_block(rel, body, bools, reset, sup, out);
        }
        Expr::If(ExprIf {
            then_branch,
            else_branch,
            ..
        }) => {
            scan_block(rel, then_branch, bools, reset, sup, out);
            if let Some((_, els)) = else_branch {
                inspect_expr(rel, els, bools, reset, sup, out);
            }
        }
        Expr::Block(b) => scan_block(rel, &b.block, bools, reset, sup, out),
        _ => {}
    }
}

fn check_loop(
    rel: &str,
    body: &Block,
    line: usize,
    bools: &HashSet<String>,
    reset: &HashSet<String>,
    sup: &Suppressions,
    out: &mut Vec<Violation>,
) {
    let mut hits: Vec<String> = Vec::new();
    let mut v = WriteScanner {
        bools,
        in_if: 0,
        hits: &mut hits,
    };
    v.visit_block(body);
    let read = read_idents(body);
    for name in hits {
        if reset.contains(&name) {
            continue;
        }
        if read.contains(&name) {
            continue;
        }
        if sup.is_suppressed(line, ID) {
            continue;
        }
        let key = format!("{rel}:{line}:{name}");
        out.push(Violation::warn(
            ID,
            key,
            format!(
                "`{name} = true` set under `if` inside a loop is a hand-rolled \
                 `iter().any(...)`. Replace with `let {name} = it.any(|x| pred(x))` \
                 (or `{name} |= it.any(...)` if the loop has other side-effects)."
            ),
        ));
    }
}

/// Identifiers read as an r-value inside the loop body. A find-first latch
/// is write-only within the loop (set, never consumed) and read only
/// *after* the loop. A flag read inside the loop — e.g. as its own
/// re-entry guard `if !flag && pred { ...; flag = true }` — participates in
/// loop control and cannot be folded into `.any()` without changing
/// behaviour, so it must not be flagged.
fn read_idents(body: &Block) -> HashSet<String> {
    struct Reads {
        names: HashSet<String>,
    }
    impl<'ast> Visit<'ast> for Reads {
        fn visit_expr_assign(&mut self, e: &'ast ExprAssign) {
            self.visit_expr(&e.right);
        }
        fn visit_expr_binary(&mut self, e: &'ast ExprBinary) {
            if matches!(e.op, BinOp::BitOrAssign(_) | BinOp::BitAndAssign(_)) {
                self.visit_expr(&e.right);
            } else {
                self.visit_expr(&e.left);
                self.visit_expr(&e.right);
            }
        }
        fn visit_expr_path(&mut self, p: &'ast ExprPath) {
            if let Some(name) = single_ident_path(p) {
                self.names.insert(name);
            }
        }
    }
    let mut r = Reads {
        names: HashSet::new(),
    };
    r.visit_block(body);
    r.names
}

struct WriteScanner<'a> {
    bools: &'a HashSet<String>,
    in_if: usize,
    hits: &'a mut Vec<String>,
}

impl<'ast> Visit<'ast> for WriteScanner<'_> {
    fn visit_expr_if(&mut self, e: &'ast ExprIf) {
        self.in_if += 1;
        syn::visit::visit_expr_if(self, e);
        self.in_if -= 1;
    }
    fn visit_expr_assign(&mut self, e: &'ast ExprAssign) {
        if self.in_if > 0
            && let Some(name) = single_ident(&e.left)
            && self.bools.contains(&name)
            && matches!(&*e.right, Expr::Lit(l) if matches!(&l.lit, Lit::Bool(b) if b.value))
        {
            self.hits.push(name);
        }
        syn::visit::visit_expr_assign(self, e);
    }
    fn visit_expr_binary(&mut self, e: &'ast ExprBinary) {
        if self.in_if > 0
            && matches!(e.op, BinOp::BitOrAssign(_))
            && let Some(name) = single_ident(&e.left)
            && self.bools.contains(&name)
            && matches!(&*e.right, Expr::Lit(l) if matches!(&l.lit, Lit::Bool(b) if b.value))
        {
            self.hits.push(name);
        }
        syn::visit::visit_expr_binary(self, e);
    }
}

fn is_false_lit(e: &Expr) -> bool {
    matches!(e, Expr::Lit(l) if matches!(&l.lit, Lit::Bool(b) if !b.value))
}

fn single_ident(expr: &Expr) -> Option<String> {
    let Expr::Path(p) = expr else { return None };
    single_ident_path(p)
}

fn single_ident_path(p: &ExprPath) -> Option<String> {
    if p.qself.is_some() || p.path.segments.len() != 1 {
        return None;
    }
    Some(p.path.segments[0].ident.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count(src: &str) -> usize {
        let suppress = Suppressions::parse(src);
        let file: syn::File = syn::parse_str(src).expect("valid Rust source");
        let mut out = Vec::new();
        analyze_file("fixture.rs", &file, &suppress, &mut out);
        out.len()
    }

    #[test]
    fn flag_set_under_if_under_for_flagged() {
        let src = "fn x(xs: &[i32]) -> bool { \
                   let mut started = false; \
                   for v in xs { if *v > 0 { started = true; } } \
                   started }";
        assert_eq!(count(src), 1);
    }

    #[test]
    fn flag_or_assign_also_flagged() {
        let src = "fn x(xs: &[i32]) -> bool { \
                   let mut started = false; \
                   for v in xs { if *v > 0 { started |= true; } } \
                   started }";
        assert_eq!(count(src), 1);
    }

    #[test]
    fn flag_set_to_false_not_flagged() {
        let src = "fn x(xs: &[i32]) -> bool { \
                   let mut ok = true; \
                   for v in xs { if *v < 0 { ok = false; } } \
                   ok }";
        assert_eq!(count(src), 0);
    }

    #[test]
    fn flag_outside_function_scope_not_flagged() {
        let src = "fn x(this: &mut S, xs: &[i32]) { \
                   for v in xs { if *v > 0 { this.started = true; } } }";
        assert_eq!(count(src), 0);
    }

    #[test]
    fn flag_set_outside_if_not_flagged() {
        let src = "fn x(xs: &[i32]) -> bool { \
                   let mut started = false; \
                   for _ in xs { started = true; } \
                   started }";
        assert_eq!(count(src), 0);
    }

    #[test]
    fn flag_reset_to_false_elsewhere_not_flagged() {
        let src = "fn x(items: &[Item]) { \
                   let mut logged = false; \
                   loop { \
                       match tick() { \
                           Ok(()) => logged = false, \
                           Err(e) => { if !logged { log(e); logged = true; } } \
                       } \
                   } }";
        assert_eq!(count(src), 0);
    }

    #[test]
    fn flag_read_inside_loop_as_guard_not_flagged() {
        let src = "fn x() { \
                   let mut marked = false; \
                   while let Ok(ev) = recv() { \
                       if !marked && pred(ev) { side_effect(); marked = true; } \
                   } }";
        assert_eq!(count(src), 0);
    }

    #[test]
    fn write_only_flag_still_flagged_with_side_effect() {
        let src = "fn x(xs: &[i32]) -> bool { \
                   let mut found = false; \
                   for v in xs { if *v > 0 { side_effect(*v); found = true; } } \
                   found }";
        assert_eq!(count(src), 1);
    }
}

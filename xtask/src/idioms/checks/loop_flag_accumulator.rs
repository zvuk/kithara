//! Flag hand-rolled `iter::any` — `flag = true` (or `flag |= true`)
//! inside an `if` inside a `for`/`while`/`loop`, where `flag` is bound
//! by `let mut name = bool_lit;` outside the loop. Suggest replacing
//! with `let flag = it.any(|x| pred(x))`.

use std::collections::HashSet;

use anyhow::Result;
use syn::{
    BinOp, Block, Expr, ExprAssign, ExprBinary, ExprForLoop, ExprIf, ExprLoop, ExprWhile, Lit,
    Local, Pat, PatIdent, Stmt, visit::Visit,
};

use super::{Check, Context};
use crate::common::{
    parse::parse_file,
    suppress::Suppressions,
    violation::Violation,
    walker::{compile_globs, matches_any, relative_to, workspace_rs_files_scoped},
};

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
            scan_block(self.rel, &f.block, &HashSet::new(), self.sup, self.out);
            syn::visit::visit_item_fn(self, f);
        }
        fn visit_impl_item_fn(&mut self, f: &'ast syn::ImplItemFn) {
            scan_block(self.rel, &f.block, &HashSet::new(), self.sup, self.out);
            syn::visit::visit_impl_item_fn(self, f);
        }
    }
    let mut v = V { rel, sup, out };
    v.visit_file(file);
}

fn scan_block(
    rel: &str,
    block: &Block,
    parent_bools: &HashSet<String>,
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
            inspect_expr(rel, expr, &bools, sup, out);
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
    sup: &Suppressions,
    out: &mut Vec<Violation>,
) {
    match expr {
        Expr::ForLoop(ExprForLoop {
            body, for_token, ..
        }) => {
            check_loop(rel, body, for_token.span.start().line, bools, sup, out);
            scan_block(rel, body, bools, sup, out);
        }
        Expr::While(ExprWhile {
            body, while_token, ..
        }) => {
            check_loop(rel, body, while_token.span.start().line, bools, sup, out);
            scan_block(rel, body, bools, sup, out);
        }
        Expr::Loop(ExprLoop {
            body, loop_token, ..
        }) => {
            check_loop(rel, body, loop_token.span.start().line, bools, sup, out);
            scan_block(rel, body, bools, sup, out);
        }
        Expr::If(ExprIf {
            then_branch,
            else_branch,
            ..
        }) => {
            scan_block(rel, then_branch, bools, sup, out);
            if let Some((_, els)) = else_branch {
                inspect_expr(rel, els, bools, sup, out);
            }
        }
        Expr::Block(b) => scan_block(rel, &b.block, bools, sup, out),
        _ => {}
    }
}

fn check_loop(
    rel: &str,
    body: &Block,
    line: usize,
    bools: &HashSet<String>,
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
    for name in hits {
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

fn single_ident(expr: &Expr) -> Option<String> {
    let Expr::Path(p) = expr else { return None };
    if p.path.segments.len() != 1 {
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
}

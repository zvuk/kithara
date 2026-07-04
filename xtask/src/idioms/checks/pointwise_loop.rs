use anyhow::Result;
use kithara_xtask_core::common::{
    parse::parse_file,
    suppress::Suppressions,
    violation::Violation,
    walker::{compile_globs, matches_any, relative_to, workspace_rs_files_scoped},
};
use syn::{BinOp, Block, Expr, ExprForLoop, Stmt, visit::Visit};

use super::{Check, Context};

pub(crate) const ID: &str = "pointwise_loop";

pub(crate) struct PointwiseLoop;

impl Check for PointwiseLoop {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.pointwise_loop;
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
        fn visit_expr_for_loop(&mut self, fl: &'ast ExprForLoop) {
            if is_pointwise_loop(fl) {
                let line = fl.for_token.span.start().line;
                if !self.sup.is_suppressed(line, ID) {
                    let key = format!("{}:{}:pointwise_for", self.rel, line);
                    self.out.push(Violation::warn(
                        ID,
                        key,
                        "for-loop is a pointwise binary op over zipped iterators; \
                         replace with `.for_each(|(a, &b)| *a OP= b)`. In hot paths \
                         consider a SIMD primitive. If the iterator chain ends in \
                         `.take(N)`, also slice the operands to `[..N]` first to \
                         enable bounds-check elision."
                            .to_string(),
                    ));
                }
            }
            syn::visit::visit_expr_for_loop(self, fl);
        }
    }
    let mut v = V { rel, sup, out };
    v.visit_file(file);
}

fn is_pointwise_loop(fl: &ExprForLoop) -> bool {
    if !iter_is_zip_like(&fl.expr) {
        return false;
    }
    is_pointwise_body(&fl.body)
}

/// True if the iterator's outermost method-call chain ends in `.zip(...)`
/// (possibly wrapped by `.take(...)` / `.enumerate()` / `.rev()` / `.skip(...)`).
fn iter_is_zip_like(expr: &Expr) -> bool {
    let mut cur = expr;
    loop {
        let Expr::MethodCall(m) = cur else {
            return false;
        };
        let name = m.method.to_string();
        if name == "zip" {
            return true;
        }
        if matches!(name.as_str(), "take" | "enumerate" | "rev" | "skip") {
            cur = &m.receiver;
            continue;
        }
        return false;
    }
}

fn is_pointwise_body(block: &Block) -> bool {
    if block.stmts.len() != 1 {
        return false;
    }
    let stmt = &block.stmts[0];
    let Stmt::Expr(expr, _) = stmt else {
        return false;
    };
    match expr {
        Expr::Assign(a) => is_deref_or_index(&a.left),
        _ => is_assign_op(expr),
    }
}

fn is_deref_or_index(e: &Expr) -> bool {
    matches!(e, Expr::Unary(u) if matches!(u.op, syn::UnOp::Deref(_)))
        || matches!(e, Expr::Index(_))
}

fn is_assign_op(expr: &Expr) -> bool {
    let Expr::Binary(b) = expr else {
        return false;
    };
    matches!(
        b.op,
        BinOp::AddAssign(_)
            | BinOp::SubAssign(_)
            | BinOp::MulAssign(_)
            | BinOp::DivAssign(_)
            | BinOp::RemAssign(_)
            | BinOp::BitXorAssign(_)
            | BinOp::BitAndAssign(_)
            | BinOp::BitOrAssign(_)
            | BinOp::ShlAssign(_)
            | BinOp::ShrAssign(_)
    ) && is_deref_or_index(&b.left)
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
    fn pointwise_add_flagged() {
        let src = "fn x(o: &mut [f32], m: &[f32], n: usize) { \
                   for (o, &m) in o.iter_mut().zip(m.iter()).take(n) { *o += m; } }";
        assert_eq!(count(src), 1);
    }

    #[test]
    fn pointwise_assign_flagged() {
        let src = "fn x(o: &mut [u8], m: &[u8]) { \
                   for (o, &m) in o.iter_mut().zip(m.iter()) { *o = m; } }";
        assert_eq!(count(src), 1);
    }

    #[test]
    fn body_with_branch_not_flagged() {
        let src = "fn x(o: &mut [f32], m: &[f32]) { \
                   for (o, &m) in o.iter_mut().zip(m.iter()) { \
                       if m > 0.0 { *o += m; } } }";
        assert_eq!(count(src), 0);
    }

    #[test]
    fn body_two_stmts_not_flagged() {
        let src = "fn x(o: &mut [f32], m: &[f32]) { \
                   for (o, &m) in o.iter_mut().zip(m.iter()) { let t = m; *o += t; } }";
        assert_eq!(count(src), 0);
    }

    #[test]
    fn loop_without_zip_not_flagged() {
        let src = "fn x(o: &mut [f32]) { for v in o.iter_mut() { *v += 1.0; } }";
        assert_eq!(count(src), 0);
    }

    #[test]
    fn nested_loops_flag_only_inner() {
        let src = "fn x(out: &mut [Vec<f32>], mix: &[Vec<f32>], n: usize) { \
                   for (out_ch, mix_ch) in out.iter_mut().zip(mix.iter()) { \
                       for (o, &m) in out_ch.iter_mut().zip(mix_ch.iter()).take(n) { \
                           *o += m; \
                       } \
                   } }";
        assert_eq!(count(src), 1);
    }
}

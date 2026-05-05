//! Flag for/while/loop blocks whose body is a fat aggregator.
//!
//! Heuristic: body has more statements than the per-kind threshold
//! (for/while=6, unconditional loop=4 by default) AND contains at
//! least one nested control-flow statement (if/match/inner loop).
//! Suggest extracting the body into a named function and reducing
//! the loop body to a sequence of calls.

use anyhow::Result;
use syn::{Block, Expr, ExprForLoop, ExprLoop, ExprWhile, Stmt, visit::Visit};

use super::{Check, Context};
use crate::{
    idioms::config::FatLoopBodyConfig,
    common::{
        parse::parse_file,
        suppress::Suppressions,
        violation::Violation,
        walker::{compile_globs, matches_any, relative_to, workspace_rs_files_scoped},
    },
};

pub(crate) const ID: &str = "fat_loop_body";

pub(crate) struct FatLoopBody;

impl Check for FatLoopBody {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.fat_loop_body;
        let exempt = compile_globs(&cfg.exempt_files);
        let mut violations = Vec::new();
        for path in workspace_rs_files_scoped(ctx.workspace_root, ctx.scope)? {
            let rel = relative_to(ctx.workspace_root, &path);
            if matches_any(&exempt, rel) {
                continue;
            }
            let Ok(file) = parse_file(&path) else { continue };
            let src = std::fs::read_to_string(&path)?;
            let suppress = Suppressions::parse(&src);
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            analyze_file(&rel_str, &file, cfg, &suppress, &mut violations);
        }
        violations.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(violations)
    }
}

fn analyze_file(
    rel: &str,
    file: &syn::File,
    cfg: &FatLoopBodyConfig,
    sup: &Suppressions,
    out: &mut Vec<Violation>,
) {
    struct V<'a> {
        rel: &'a str,
        cfg: &'a FatLoopBodyConfig,
        sup: &'a Suppressions,
        out: &'a mut Vec<Violation>,
    }
    impl<'ast> Visit<'ast> for V<'_> {
        fn visit_expr_for_loop(&mut self, fl: &'ast ExprForLoop) {
            self.check(&fl.body, fl.for_token.span.start().line, "for");
            syn::visit::visit_expr_for_loop(self, fl);
        }
        fn visit_expr_while(&mut self, w: &'ast ExprWhile) {
            self.check(&w.body, w.while_token.span.start().line, "while");
            syn::visit::visit_expr_while(self, w);
        }
        fn visit_expr_loop(&mut self, l: &'ast ExprLoop) {
            self.check(&l.body, l.loop_token.span.start().line, "loop");
            syn::visit::visit_expr_loop(self, l);
        }
    }
    impl V<'_> {
        fn check(&mut self, body: &Block, line: usize, kind: &str) {
            let stmt_count = body.stmts.len();
            let threshold = match kind {
                "for" => self.cfg.for_stmt_threshold,
                "while" => self.cfg.while_stmt_threshold,
                "loop" => self.cfg.loop_stmt_threshold,
                _ => self.cfg.for_stmt_threshold,
            };
            if stmt_count <= threshold {
                return;
            }
            let nested = count_nested_ctrl(body);
            if nested < self.cfg.nested_ctrl_threshold {
                return;
            }
            if self.sup.is_suppressed(line, ID) {
                return;
            }
            let key = format!("{}:{}:{kind}_body", self.rel, line);
            self.out.push(Violation::warn(
                ID,
                key,
                format!(
                    "`{kind}` body has {stmt_count} statements (threshold {threshold}) and \
                     {nested} nested control-flow expression(s); extract the body into named \
                     functions (`fn drain_xxx(...)`, `fn tick_xxx(...)`) and reduce the body \
                     to a sequence of those calls. Tick-driver `loop {{}}` bodies should be \
                     3–4 calls maximum."
                ),
            ));
        }
    }
    let mut v = V {
        rel,
        cfg,
        sup,
        out,
    };
    v.visit_file(file);
}

fn count_nested_ctrl(body: &Block) -> usize {
    let mut n = 0;
    for stmt in &body.stmts {
        let expr = match stmt {
            Stmt::Expr(e, _) => e,
            Stmt::Local(loc) => match &loc.init {
                Some(init) => &init.expr,
                None => continue,
            },
            _ => continue,
        };
        if matches!(
            expr,
            Expr::ForLoop(_) | Expr::While(_) | Expr::Loop(_) | Expr::If(_) | Expr::Match(_)
        ) {
            n += 1;
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> FatLoopBodyConfig {
        FatLoopBodyConfig {
            for_stmt_threshold: 5,
            while_stmt_threshold: 5,
            loop_stmt_threshold: 3,
            nested_ctrl_threshold: 1,
            exempt_files: Vec::new(),
        }
    }

    fn count(src: &str) -> usize {
        let cfg = cfg();
        let suppress = Suppressions::parse(src);
        let file: syn::File = syn::parse_str(src).expect("valid Rust source");
        let mut out = Vec::new();
        analyze_file("fixture.rs", &file, &cfg, &suppress, &mut out);
        out.len()
    }

    #[test]
    fn fat_body_with_nested_for_flagged() {
        let src = "fn x(xs: &mut [i32], ys: &[i32]) { \
                   for x in xs.iter_mut() { \
                       *x = 0; *x += 1; *x += 2; *x += 3; *x += 4; \
                       for y in ys { *x += *y; } \
                   } }";
        assert_eq!(count(src), 1);
    }

    #[test]
    fn fat_body_no_nested_ctrl_clean() {
        let src = "fn x(xs: &mut [i32]) { \
                   for x in xs.iter_mut() { \
                       *x = 0; *x += 1; *x += 2; *x += 3; *x += 4; *x += 5; *x += 6; \
                   } }";
        assert_eq!(count(src), 0);
    }

    #[test]
    fn short_body_with_nested_clean() {
        let src = "fn x(xs: &mut [i32]) { \
                   for x in xs.iter_mut() { \
                       if *x > 0 { *x -= 1; } \
                   } }";
        assert_eq!(count(src), 0);
    }

    #[test]
    fn while_loop_also_flagged() {
        let src = "fn x(c: &mut i32) { \
                   while *c < 10 { \
                       *c += 1; *c += 1; *c += 1; *c += 1; *c += 1; \
                       if *c == 5 { *c = 0; } \
                   } }";
        assert_eq!(count(src), 1);
    }

    #[test]
    fn unconditional_loop_uses_tighter_threshold() {
        let src = "fn x(state: &mut i32) { \
                   loop { \
                       *state += 1; *state += 1; *state += 1; \
                       if *state > 100 { break; } \
                   } }";
        assert_eq!(count(src), 1);
    }
}

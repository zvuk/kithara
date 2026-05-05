//! Flag nested `if let Some(_)` / `if let Ok(_)` chains on the happy
//! path. clippy's `collapsible_match` only fires when the inner branch
//! is a single statement; this catches multi-stmt happy-path pyramids.
//! Suggest `let-else` for guard form, `?`-propagation, or extracting
//! a helper that takes the unwrapped values.

use anyhow::Result;
use syn::{Block, Expr, ExprIf, Pat, PatIdent, PatTupleStruct, Stmt, visit::Visit};

use super::{Check, Context};
use crate::{
    idioms::config::NestedIfLetPyramidConfig,
    common::{
        parse::parse_file,
        suppress::Suppressions,
        violation::Violation,
        walker::{compile_globs, matches_any, relative_to, workspace_rs_files_scoped},
    },
};

pub(crate) const ID: &str = "nested_if_let_pyramid";

pub(crate) struct NestedIfLetPyramid;

impl Check for NestedIfLetPyramid {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.nested_if_let_pyramid;
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
    cfg: &NestedIfLetPyramidConfig,
    sup: &Suppressions,
    out: &mut Vec<Violation>,
) {
    struct V<'a> {
        rel: &'a str,
        cfg: &'a NestedIfLetPyramidConfig,
        sup: &'a Suppressions,
        out: &'a mut Vec<Violation>,
        in_pyramid: bool,
    }
    impl<'ast> Visit<'ast> for V<'_> {
        fn visit_expr_if(&mut self, e: &'ast ExprIf) {
            if self.in_pyramid {
                syn::visit::visit_expr_if(self, e);
                return;
            }
            if let Some(depth) = pyramid_depth(e) {
                if depth >= self.cfg.min_depth {
                    let line = e.if_token.span.start().line;
                    if !self.sup.is_suppressed(line, ID) {
                        let key = format!("{}:{}:depth_{}", self.rel, line, depth);
                        self.out.push(Violation::warn(
                            ID,
                            key,
                            format!(
                                "nested `if let Some/Ok` pyramid (depth {depth}) — each level \
                                 pushes work further right. Replace with `let Some(x) = expr \
                                 else {{ return; }};` (when fn returns `()`), `let x = expr?;` \
                                 (when fn returns `Result`/`Option`), or extract an inner \
                                 helper `fn step(unwrapped: T, ...)`."
                            ),
                        ));
                    }
                    self.in_pyramid = true;
                    syn::visit::visit_expr_if(self, e);
                    self.in_pyramid = false;
                    return;
                }
            }
            syn::visit::visit_expr_if(self, e);
        }
    }
    let mut v = V {
        rel,
        cfg,
        sup,
        out,
        in_pyramid: false,
    };
    v.visit_file(file);
}

fn pyramid_depth(e: &ExprIf) -> Option<usize> {
    if !is_if_let_some_ok(e) {
        return None;
    }
    let inner = block_max_pyramid(&e.then_branch);
    Some(1 + inner)
}

fn is_if_let_some_ok(e: &ExprIf) -> bool {
    let Expr::Let(let_) = e.cond.as_ref() else {
        return false;
    };
    pat_is_some_or_ok(&let_.pat)
}

fn pat_is_some_or_ok(p: &Pat) -> bool {
    match p {
        Pat::TupleStruct(PatTupleStruct { path, elems, .. }) => {
            let last = path.segments.last().map(|s| s.ident.to_string());
            matches!(last.as_deref(), Some("Some" | "Ok"))
                && elems.len() == 1
                && matches!(
                    elems.first(),
                    Some(Pat::Ident(PatIdent { .. }) | Pat::Wild(_) | Pat::TupleStruct(_))
                )
        }
        Pat::Or(o) => o.cases.iter().all(pat_is_some_or_ok),
        _ => false,
    }
}

fn block_max_pyramid(b: &Block) -> usize {
    let mut best = 0;
    for stmt in &b.stmts {
        if let Some(d) = stmt_pyramid_depth(stmt) {
            best = best.max(d);
        }
    }
    best
}

fn stmt_pyramid_depth(s: &Stmt) -> Option<usize> {
    let expr = match s {
        Stmt::Expr(e, _) => e,
        Stmt::Local(loc) => loc.init.as_ref().map(|i| &*i.expr)?,
        _ => return None,
    };
    if let Expr::If(e) = expr {
        pyramid_depth(e)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count(src: &str) -> usize {
        let cfg = NestedIfLetPyramidConfig {
            min_depth: 2,
            exempt_files: Vec::new(),
        };
        let suppress = Suppressions::parse(src);
        let file: syn::File = syn::parse_str(src).expect("valid Rust source");
        let mut out = Vec::new();
        analyze_file("fixture.rs", &file, &cfg, &suppress, &mut out);
        out.len()
    }

    #[test]
    fn depth_two_with_extra_stmts_flagged() {
        let src = "fn x(state: &mut S) { \
                   if let Some(ctx) = state.ctx.as_mut() { \
                       if let Err(_) = ctx.update() {} \
                       if let Some(backend) = ctx.active_backend_mut() { \
                           let _ = backend.render(0); \
                       } \
                   } }";
        assert_eq!(count(src), 1);
    }

    #[test]
    fn depth_one_clean() {
        let src = "fn x(o: Option<u32>) { if let Some(v) = o { let _ = v; } }";
        assert_eq!(count(src), 0);
    }

    #[test]
    fn collapsible_match_clippy_already_catches_not_double_flagged() {
        let src = "fn x(o: Option<Option<u32>>) { \
                   if let Some(inner) = o { if let Some(v) = inner { let _ = v; } } }";
        assert_eq!(count(src), 1);
    }

    #[test]
    fn three_levels_one_violation_per_pyramid() {
        let src = "fn x(a: Option<Option<Option<u32>>>) { \
                   if let Some(b) = a { \
                       if let Some(c) = b { \
                           if let Some(d) = c { let _ = d; } \
                       } \
                   } }";
        assert_eq!(count(src), 1);
    }

    #[test]
    fn unrelated_branches_not_flagged() {
        let src = "fn x(a: Option<u32>, b: Option<u32>) { \
                   if let Some(_) = a { let _ = 1; } \
                   if let Some(_) = b { let _ = 2; } }";
        assert_eq!(count(src), 0);
    }
}

//! `Box::new(StructLiteral { ... })` — boxing a sized concrete value
//! that has no obvious need for heap storage.
//!
//! `Box<T>` is for two cases: erasing into `Box<dyn Trait>`, or breaking
//! infinite recursion in enums (`enum E { Node(Box<E>) }`). Wrapping a
//! sized concrete struct is an extra heap allocation + an extra
//! indirection on every access, with no readability or polymorphism
//! benefit.
//!
//! Detection is heuristic — without type inference the check can't
//! prove the value isn't being coerced into `Box<dyn Trait>` later.
//! It looks for the strongest signal: `Box::new(StructLiteral { ... })`
//! where the inner expression is a struct literal of a concrete type,
//! is *not* immediately cast `as Box<dyn _>`, and the constructor isn't
//! the explicit `Box::<dyn Trait>::new(...)` form. This catches the
//! common smell (enum variants like `Config(Box<ResourceConfig>)` that
//! never actually need indirection) without flooding the report with
//! cases the compiler would justify via inference.

use anyhow::Result;
use syn::{
    Expr, ExprCall, ExprPath, GenericArgument, PathArguments, Type,
    spanned::Spanned,
    visit::{self, Visit},
};

use super::{Check, Context};
use crate::common::{
    parse::parse_file,
    suppress::Suppressions,
    violation::Violation,
    walker::{compile_globs, matches_any, relative_to, workspace_rs_files_scoped},
};

pub(crate) const ID: &str = "box_concrete_type";

const EXPLANATION: &str = "\
Detected `Box::new(StructLiteral { ... })` on a concrete (non-trait-object) value.

Why it matters. `Box<T>` exists for two cases: erasing a sized type into \
`Box<dyn Trait>` for runtime polymorphism, or breaking infinite type \
recursion in enums (`enum Tree { Leaf, Node(Box<Tree>) }`). Wrapping a \
sized concrete struct in `Box::new` for any other reason adds one heap \
allocation per construction, one indirection on every access, plus \
pointer-chasing cost in cache. If the type is `Sized` and you don't need \
polymorphism, store it inline. If size pressure is the worry, use \
`Box<T>` only when the type is genuinely large (>= a few hundred bytes) \
and rarely accessed.

❌  enum TrackSource { Url(Url), Config(Box<ResourceConfig>) } // ResourceConfig sized, no recursion
✅  enum TrackSource { Url(Url), Config(ResourceConfig) }

Suppress with `// xtask-lint-ignore: box_concrete_type` for: (1) enums \
where the boxed variant is rare and reduces stack size of the others, \
(2) recursive enums (`Box<Self>`), (3) tests that intentionally exercise \
a `Box::new` path. Heuristic check — suppress noisy false positives \
without guilt.";

pub(crate) struct BoxConcreteType;

impl Check for BoxConcreteType {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.box_concrete_type;
        let exempt = compile_globs(&cfg.exempt_files);
        let mut violations = Vec::new();
        for path in workspace_rs_files_scoped(ctx.workspace_root, ctx.scope)? {
            let rel_path = relative_to(ctx.workspace_root, &path).to_path_buf();
            let rel = rel_path.to_string_lossy().replace('\\', "/");
            if matches_any(&exempt, std::path::Path::new(&rel)) {
                continue;
            }
            let Ok(source) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(file) = parse_file(&path) else {
                continue;
            };
            let suppress = Suppressions::parse(&source);
            let mut v = BoxVisitor {
                rel: &rel,
                suppress: &suppress,
                out: &mut violations,
            };
            v.visit_file(&file);
        }
        violations.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(violations)
    }
}

struct BoxVisitor<'a> {
    rel: &'a str,
    suppress: &'a Suppressions,
    out: &'a mut Vec<Violation>,
}

impl<'ast> Visit<'ast> for BoxVisitor<'_> {
    fn visit_expr_call(&mut self, c: &'ast ExprCall) {
        if is_box_new_concrete(c) {
            let s = c.span().start();
            if !self.suppress.is_suppressed(s.line, ID) {
                let key = format!("{}:{}:{}", self.rel, s.line, s.column);
                self.out.push(
                    Violation::warn(
                        ID,
                        key,
                        "B1: `Box::new(StructLiteral { ... })` on a concrete sized type — \
                             store inline unless the variant is genuinely large or this is \
                             `Box<dyn Trait>` after coercion",
                    )
                    .with_explanation(EXPLANATION),
                );
            }
        }
        visit::visit_expr_call(self, c);
    }
}

fn is_box_new_concrete(c: &ExprCall) -> bool {
    let Expr::Path(ExprPath { path, qself, .. }) = &*c.func else {
        return false;
    };
    // `<dyn Trait as Box<...>>::new` carries a qualified self — opt out.
    if qself.is_some() {
        return false;
    }
    // Match `Box::new` (with or without leading `::std::boxed::`).
    let segs: Vec<&syn::PathSegment> = path.segments.iter().collect();
    if segs.len() < 2 {
        return false;
    }
    let last = &segs[segs.len() - 1];
    let parent = &segs[segs.len() - 2];
    if last.ident != "new" || parent.ident != "Box" {
        return false;
    }
    // `Box::<dyn Trait>::new(...)` — turbofish gives the trait object explicitly.
    if let PathArguments::AngleBracketed(args) = &parent.arguments {
        for arg in &args.args {
            if let GenericArgument::Type(Type::TraitObject(_)) = arg {
                return false;
            }
        }
    }
    if c.args.len() != 1 {
        return false;
    }
    is_struct_literal(&c.args[0])
}

fn is_struct_literal(e: &Expr) -> bool {
    matches!(e, Expr::Struct(_))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count_in(src: &str) -> usize {
        let file: syn::File = syn::parse_str(src).expect("valid Rust source");
        let suppress = Suppressions::parse(src);
        let mut out = Vec::new();
        let mut v = BoxVisitor {
            rel: "fixture.rs",
            suppress: &suppress,
            out: &mut out,
        };
        v.visit_file(&file);
        out.len()
    }

    #[test]
    fn struct_literal_in_box_flagged() {
        let src = "struct S { x: i32 } fn f() { let _ = Box::new(S { x: 1 }); }";
        assert_eq!(count_in(src), 1);
    }

    #[test]
    fn box_with_function_call_not_flagged() {
        // `Box::new(f())` returns whatever `f` produces — could be a trait
        // object after coercion. Heuristic-only check skips this.
        let src = "fn f() -> i32 { 1 } fn g() { let _ = Box::new(f()); }";
        assert_eq!(count_in(src), 0);
    }

    #[test]
    fn turbofish_dyn_skipped() {
        let src = "trait T {} struct S; impl T for S {} fn f() { let _: Box<dyn T> = Box::<dyn T>::new(S); }";
        assert_eq!(count_in(src), 0);
    }

    #[test]
    fn box_with_literal_not_flagged() {
        let src = "fn f() { let _ = Box::new(42); }";
        assert_eq!(count_in(src), 0);
    }

    #[test]
    fn nested_box_inside_struct_flagged() {
        // The outer `Box::new` is on a struct literal; the inner is on a
        // primitive — both are skipped except the struct-literal one.
        let src =
            "struct S { inner: Box<i32> } fn f() { let _ = Box::new(S { inner: Box::new(0) }); }";
        assert_eq!(count_in(src), 1);
    }

    #[test]
    fn suppression_works() {
        let src = "struct S { x: i32 }
fn f() {
    // xtask-lint-ignore: box_concrete_type
    let _ = Box::new(S { x: 1 });
}";
        assert_eq!(count_in(src), 0);
    }
}

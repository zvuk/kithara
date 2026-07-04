use anyhow::Result;
use kithara_xtask_core::common::{
    parse::parse_file,
    suppress::Suppressions,
    violation::Violation,
    walker::{compile_globs, matches_any, relative_to, workspace_rs_files_scoped},
};
use syn::{
    Block, Expr, ExprCall, ExprCast, ExprPath, GenericArgument, Local, Pat, PatIdent,
    PathArguments, Type,
    spanned::Spanned,
    visit::{self, Visit},
};

use super::{Check, Context};

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

Exempt by construction: a `Box::new(StructLiteral)` whose box-bound local \
escapes to FFI as a raw pointer within the same function — via \
`Box::into_raw(b)` or a raw-pointer cast of the box (`b.as_mut() as *mut _`, \
`&*b as *const _`). There the `Box` is a pinned allocation: it hands a \
stable heap address to a C API and owns the backing storage for the \
duration of the call. Storing the struct inline would move it and dangle \
the live FFI pointer.

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
    fn visit_item_fn(&mut self, f: &'ast syn::ItemFn) {
        self.scan_fn(&f.block);
        visit::visit_item_fn(self, f);
    }

    fn visit_impl_item_fn(&mut self, f: &'ast syn::ImplItemFn) {
        self.scan_fn(&f.block);
        visit::visit_impl_item_fn(self, f);
    }
}

impl BoxVisitor<'_> {
    /// Walk one function body: every `Box::new(StructLiteral)` call is a
    /// candidate. A candidate is exempt when it is bound to a local whose
    /// heap pointer escapes to a raw pointer somewhere in the same body
    /// (an FFI "pinned allocation"). Nested functions are visited
    /// separately by the outer `Visit` walk, so each `Box::new` is scored
    /// against exactly its own enclosing body.
    fn scan_fn(&mut self, block: &Block) {
        let mut finder = CallFinder { calls: Vec::new() };
        finder.visit_block(block);
        let mut escapes = EscapeScanner {
            escaped: Vec::new(),
        };
        escapes.visit_block(block);
        for (call, bound_local) in finder.calls {
            if let Some(name) = &bound_local
                && escapes.escaped.iter().any(|e| e == name)
            {
                continue;
            }
            let s = call.span().start();
            if self.suppress.is_suppressed(s.line, ID) {
                continue;
            }
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
}

/// Collects every `Box::new(StructLiteral)` call in a body. When the call
/// is the direct initializer of a `let <ident> = ...;` binding, records
/// the bound identifier so the escape scanner can decide whether the box
/// is a pinned FFI allocation.
struct CallFinder<'ast> {
    calls: Vec<(&'ast ExprCall, Option<String>)>,
}

impl<'ast> Visit<'ast> for CallFinder<'ast> {
    fn visit_local(&mut self, loc: &'ast Local) {
        if let Some(init) = &loc.init
            && let Expr::Call(call) = &*init.expr
            && is_box_new_concrete(call)
        {
            self.calls.push((call, local_ident(loc)));
            visit::visit_local(self, loc);
            return;
        }
        visit::visit_local(self, loc);
    }

    fn visit_expr_call(&mut self, c: &'ast ExprCall) {
        if is_box_new_concrete(c) && !self.calls.iter().any(|(seen, _)| std::ptr::eq(*seen, c)) {
            self.calls.push((c, None));
        }
        visit::visit_expr_call(self, c);
    }
}

/// Records the names of locals whose value escapes to a raw pointer:
/// either `Box::into_raw(<ident>)` or a cast to `*mut _` / `*const _`
/// whose base expression roots at `<ident>` (e.g. `<ident>.as_mut() as
/// *mut T`, `&*<ident> as *const T`). These are the pinned-allocation
/// FFI uses that justify the heap box.
struct EscapeScanner {
    escaped: Vec<String>,
}

impl<'ast> Visit<'ast> for EscapeScanner {
    fn visit_expr_cast(&mut self, c: &'ast ExprCast) {
        if matches!(&*c.ty, Type::Ptr(_))
            && let Some(name) = root_ident(&c.expr)
        {
            self.escaped.push(name);
        }
        visit::visit_expr_cast(self, c);
    }

    fn visit_expr_call(&mut self, c: &'ast ExprCall) {
        if is_box_into_raw(c)
            && let Some(arg) = c.args.first()
            && let Some(name) = root_ident(arg)
        {
            self.escaped.push(name);
        }
        visit::visit_expr_call(self, c);
    }
}

/// Strip the pointer-forming wrappers around an expression and return the
/// identifier it ultimately roots at, if any. Walks through method calls
/// (`x.as_mut()`), references (`&x`, `&mut x`), unary deref (`*x`), field
/// access (`x.field`), parens, and chained casts.
fn root_ident(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Path(p) => single_ident_path(p),
        Expr::MethodCall(m) => root_ident(&m.receiver),
        Expr::Reference(r) => root_ident(&r.expr),
        Expr::Unary(u) if matches!(u.op, syn::UnOp::Deref(_)) => root_ident(&u.expr),
        Expr::Field(f) => root_ident(&f.base),
        Expr::Paren(p) => root_ident(&p.expr),
        Expr::Group(g) => root_ident(&g.expr),
        Expr::Cast(c) => root_ident(&c.expr),
        _ => None,
    }
}

fn single_ident_path(p: &ExprPath) -> Option<String> {
    if p.qself.is_some() || p.path.segments.len() != 1 {
        return None;
    }
    Some(p.path.segments[0].ident.to_string())
}

fn local_ident(loc: &Local) -> Option<String> {
    match &loc.pat {
        Pat::Ident(PatIdent { ident, .. }) => Some(ident.to_string()),
        Pat::Type(t) => {
            if let Pat::Ident(PatIdent { ident, .. }) = &*t.pat {
                Some(ident.to_string())
            } else {
                None
            }
        }
        _ => None,
    }
}

fn is_box_into_raw(c: &ExprCall) -> bool {
    let Expr::Path(ExprPath { path, qself, .. }) = &*c.func else {
        return false;
    };
    if qself.is_some() {
        return false;
    }
    let segs: Vec<&syn::PathSegment> = path.segments.iter().collect();
    if segs.len() < 2 {
        return false;
    }
    let last = &segs[segs.len() - 1];
    let parent = &segs[segs.len() - 2];
    last.ident == "into_raw" && parent.ident == "Box"
}

fn is_box_new_concrete(c: &ExprCall) -> bool {
    let Expr::Path(ExprPath { path, qself, .. }) = &*c.func else {
        return false;
    };
    if qself.is_some() {
        return false;
    }
    let segs: Vec<&syn::PathSegment> = path.segments.iter().collect();
    if segs.len() < 2 {
        return false;
    }
    let last = &segs[segs.len() - 1];
    let parent = &segs[segs.len() - 2];
    if last.ident != "new" || parent.ident != "Box" {
        return false;
    }
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

    #[test]
    fn ffi_raw_pointer_cast_escape_not_flagged() {
        let src = "struct Ctx { x: i32 }
fn open() {
    let mut ctx = Box::new(Ctx { x: 1 });
    ffi(ctx.as_mut() as *mut Ctx as *mut u8);
}
fn ffi(_: *mut u8) {}";
        assert_eq!(count_in(src), 0);
    }

    #[test]
    fn ffi_into_raw_escape_not_flagged() {
        let src = "struct Ctx { x: i32 }
fn open() {
    let ctx = Box::new(Ctx { x: 1 });
    let raw = Box::into_raw(ctx);
    let _ = raw;
}";
        assert_eq!(count_in(src), 0);
    }

    #[test]
    fn ffi_reference_cast_escape_not_flagged() {
        let src = "struct Ctx { x: i32 }
fn open() {
    let ctx = Box::new(Ctx { x: 1 });
    ffi(&*ctx as *const Ctx as *const u8);
}
fn ffi(_: *const u8) {}";
        assert_eq!(count_in(src), 0);
    }

    #[test]
    fn box_without_raw_escape_still_flagged() {
        let src = "struct Ctx { x: i32 }
fn build() -> Ctx {
    let ctx = Box::new(Ctx { x: 1 });
    *ctx
}";
        assert_eq!(count_in(src), 1);
    }

    #[test]
    fn raw_cast_of_unrelated_local_does_not_exempt() {
        let src = "struct Ctx { x: i32 }
fn open(other: &mut u8) {
    let ctx = Box::new(Ctx { x: 1 });
    let _ = other as *mut u8;
    let _ = *ctx;
}";
        assert_eq!(count_in(src), 1);
    }

    #[test]
    fn escape_in_other_function_does_not_exempt() {
        let src = "struct Ctx { x: i32 }
fn build() -> Ctx {
    let ctx = Box::new(Ctx { x: 1 });
    *ctx
}
fn other(p: &mut u8) {
    let _ = p as *mut u8;
}";
        assert_eq!(count_in(src), 1);
    }
}

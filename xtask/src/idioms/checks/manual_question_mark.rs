//! Match expressions that hand-roll what the `?` operator (or
//! `Result::map_err` / `Option::map`) does in one character.
//!
//! Recognised shapes (patterns are matched modulo arm order):
//!
//! - **Q1 — bubble**: `match x { Ok(v) => v, Err(e) => return Err(e) }`
//!   → `x?`
//! - **Q2 — bubble-with-into**: `match x { Ok(v) => v, Err(e) => return Err(e.into()) }`
//!   → `x.map_err(Into::into)?`
//! - **Q3 — wrap**: `match x { Ok(v) => Ok(v), Err(e) => Err(e.into()) }`
//!   → `x.map_err(Into::into)`
//! - **Q4 — identity**: `match x { Ok(v) => Ok(v), Err(e) => Err(e) }`
//!   (and the `Some/None` mirror) — fully redundant pair.
//! - **Q5 — option-bubble**: `match opt { Some(v) => v, None => return None }`
//!   → `opt?`
//! - **Q6 — option-map**: `match opt { Some(v) => Some(f(v)), None => None }`
//!   → `opt.map(f)`
//!
//! Heuristic only: the bound identifier in the success arm must match
//! the expression returned (i.e. literally pass through). This skips
//! cases where the success arm transforms the value via an unbound
//! computation — `?` would change semantics. `// xtask-lint-ignore:
//! manual_question_mark` opts out a single match.

use anyhow::Result;
use syn::{
    Arm, Expr, ExprMatch, ExprPath, ExprReturn, Pat, PatIdent, PatStruct, PatTupleStruct, Path,
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

pub(crate) const ID: &str = "manual_question_mark";

const EXPLANATION: &str = "\
Detected a `match` expression that hand-rolls what the `?` operator (or \
`Result::map_err` / `Option::map`) does in one character.

Why it matters. Hand-rolled error propagation hides intent: a reader has \
to walk both arms to confirm \"this is just bubbling the error up\" vs \
\"this is doing something custom\". `?` has dedicated tooling support — \
it nests cleanly with `From` conversions, plays with `try_trait_v2` \
extensions, and lets `clippy::question_mark` catch related antipatterns. \
Long-form match arms also accumulate: each one is 3-5 lines vs 1 char, \
and they grow during refactoring.

❌  let body = match fetch(url) { Ok(b) => b, Err(e) => return Err(e.into()) };
✅  let body = fetch(url).map_err(Into::into)?;

Suppress with `// xtask-lint-ignore: manual_question_mark` only when both \
arms genuinely diverge in semantics (e.g. error-side does logging or \
recovery before returning).";

pub(crate) struct ManualQuestionMark;

impl Check for ManualQuestionMark {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.manual_question_mark;
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
            let mut v = MatchVisitor {
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

struct MatchVisitor<'a> {
    rel: &'a str,
    suppress: &'a Suppressions,
    out: &'a mut Vec<Violation>,
}

impl<'ast> Visit<'ast> for MatchVisitor<'_> {
    fn visit_expr_match(&mut self, m: &'ast ExprMatch) {
        if let Some(pattern) = classify(m) {
            let s = m.match_token.span().start();
            if !self.suppress.is_suppressed(s.line, ID) {
                let key = format!("{}:{}:{}", self.rel, s.line, s.column);
                self.out.push(
                    Violation::warn(ID, key, pattern.message()).with_explanation(EXPLANATION),
                );
            }
        }
        visit::visit_expr_match(self, m);
    }
}

#[derive(Debug, Clone, Copy)]
enum Pattern {
    /// `match x { Ok(v) => v, Err(e) => return Err(e) }` → `x?`
    Bubble,
    /// `match x { Ok(v) => v, Err(e) => return Err(e.into()) }` → `x.map_err(Into::into)?`
    BubbleInto,
    /// `match x { Ok(v) => Ok(v), Err(e) => Err(e.into()) }` → `x.map_err(Into::into)`
    Wrap,
    /// `match x { Ok(v) => Ok(v), Err(e) => Err(e) }` — pure identity
    Identity,
    /// `match opt { Some(v) => v, None => return None }` → `opt?`
    OptionBubble,
    /// `match opt { Some(v) => Some(f(v)), None => None }` → `opt.map(...)`
    OptionMap,
}

impl Pattern {
    fn message(self) -> &'static str {
        match self {
            Self::Bubble => "Q1: `match x { Ok(v) => v, Err(e) => return Err(e) }` — use `x?`",
            Self::BubbleInto => {
                "Q2: `match x { Ok(v) => v, Err(e) => return Err(e.into()) }` — use \
                 `x.map_err(Into::into)?`"
            }
            Self::Wrap => {
                "Q3: `match x { Ok(v) => Ok(v), Err(e) => Err(e.into()) }` — use \
                 `x.map_err(Into::into)`"
            }
            Self::Identity => {
                "Q4: identity match arms (`Ok(v) => Ok(v)`, `Err(e) => Err(e)`) — drop the match"
            }
            Self::OptionBubble => {
                "Q5: `match opt { Some(v) => v, None => return None }` — use `opt?`"
            }
            Self::OptionMap => {
                "Q6: `match opt { Some(v) => Some(f(v)), None => None }` — use `opt.map(...)`"
            }
        }
    }
}

fn classify(m: &ExprMatch) -> Option<Pattern> {
    if m.arms.len() != 2 {
        return None;
    }
    let arms = &m.arms;
    if let Some(p) = match_result(&arms[0], &arms[1]).or_else(|| match_result(&arms[1], &arms[0])) {
        return Some(p);
    }
    match_option(&arms[0], &arms[1]).or_else(|| match_option(&arms[1], &arms[0]))
}

/// Try to classify the pair as a `Result` match where `ok` is the
/// `Ok(_)` arm and `err` is the `Err(_)` arm.
fn match_result(ok: &Arm, err: &Arm) -> Option<Pattern> {
    let ok_var = pattern_single_binding(&ok.pat, "Ok")?;
    let err_var = pattern_single_binding(&err.pat, "Err")?;
    let ok_body = arm_body_expr(ok);
    let err_body = arm_body_expr(err);

    let ok_passthrough = is_ident_path(ok_body, ok_var);
    let ok_wrapped_self = is_wrap_call(ok_body, "Ok", &|e| is_ident_path(e, ok_var));
    let err_return_self = is_return_wrap_call(err_body, "Err", &|e| is_ident_path(e, err_var));
    let err_return_into = is_return_wrap_call(err_body, "Err", &|e| is_into_call(e, err_var));
    let err_wrapped_self = is_wrap_call(err_body, "Err", &|e| is_ident_path(e, err_var));
    let err_wrapped_into = is_wrap_call(err_body, "Err", &|e| is_into_call(e, err_var));

    if ok_passthrough && err_return_self {
        return Some(Pattern::Bubble);
    }
    if ok_passthrough && err_return_into {
        return Some(Pattern::BubbleInto);
    }
    if ok_wrapped_self && err_wrapped_into {
        return Some(Pattern::Wrap);
    }
    if ok_wrapped_self && err_wrapped_self {
        return Some(Pattern::Identity);
    }
    None
}

/// Try to classify the pair as an `Option` match where `some` is the
/// `Some(_)` arm and `none` is the `None` arm.
fn match_option(some: &Arm, none: &Arm) -> Option<Pattern> {
    let some_var = pattern_single_binding(&some.pat, "Some")?;
    if !pattern_is_none(&none.pat) {
        return None;
    }
    let some_body = arm_body_expr(some);
    let none_body = arm_body_expr(none);

    if is_ident_path(some_body, some_var) && is_return_none(none_body) {
        return Some(Pattern::OptionBubble);
    }
    if is_wrap_call(some_body, "Some", &|_| true) && is_none_path(none_body) {
        return Some(Pattern::OptionMap);
    }
    None
}

fn arm_body_expr(arm: &Arm) -> &Expr {
    match &*arm.body {
        // Strip a single layer of block to support `Ok(v) => { v }`.
        Expr::Block(b) if b.block.stmts.len() == 1 => match &b.block.stmts[0] {
            syn::Stmt::Expr(e, _) => e,
            _ => &arm.body,
        },
        _ => &arm.body,
    }
}

/// `Ok($var)`, `Err($var)`, `Some($var)` — one-binding tuple-struct
/// patterns. Returns the binding identifier when matched against the
/// constructor name.
fn pattern_single_binding<'a>(pat: &'a Pat, ctor: &str) -> Option<&'a syn::Ident> {
    let bindings: Vec<&syn::Ident> = match pat {
        Pat::TupleStruct(PatTupleStruct { path, elems, .. }) => {
            if !path_ends_with(path, ctor) {
                return None;
            }
            elems.iter().filter_map(pat_ident).collect()
        }
        Pat::Struct(PatStruct { path, .. }) => {
            if !path_ends_with(path, ctor) {
                return None;
            }
            return None; // struct-style patterns out of scope for now
        }
        _ => return None,
    };
    if bindings.len() == 1 {
        Some(bindings[0])
    } else {
        None
    }
}

fn pattern_is_none(pat: &Pat) -> bool {
    match pat {
        Pat::TupleStruct(PatTupleStruct { path, elems, .. }) => {
            path_ends_with(path, "None") && elems.is_empty()
        }
        Pat::Path(p) => path_ends_with(&p.path, "None"),
        Pat::Ident(PatIdent { ident, .. }) => ident == "None",
        _ => false,
    }
}

fn pat_ident(p: &Pat) -> Option<&syn::Ident> {
    if let Pat::Ident(PatIdent {
        ident,
        subpat: None,
        by_ref: None,
        mutability: None,
        ..
    }) = p
    {
        Some(ident)
    } else {
        None
    }
}

fn path_ends_with(path: &Path, name: &str) -> bool {
    path.segments
        .last()
        .is_some_and(|seg| seg.ident == name && seg.arguments.is_none())
}

fn is_ident_path(e: &Expr, ident: &syn::Ident) -> bool {
    match e {
        Expr::Path(ExprPath {
            path, qself: None, ..
        }) => path.segments.len() == 1 && path.segments[0].ident == *ident,
        _ => false,
    }
}

fn is_none_path(e: &Expr) -> bool {
    match e {
        Expr::Path(ExprPath {
            path, qself: None, ..
        }) => path_ends_with(path, "None"),
        _ => false,
    }
}

/// `Ctor(<inner>)` where `inner` matches `inner_check`.
fn is_wrap_call(e: &Expr, ctor: &str, inner_check: &dyn Fn(&Expr) -> bool) -> bool {
    let Expr::Call(call) = e else { return false };
    let Expr::Path(p) = &*call.func else {
        return false;
    };
    if !path_ends_with(&p.path, ctor) {
        return false;
    }
    if call.args.len() != 1 {
        return false;
    }
    inner_check(&call.args[0])
}

/// `return Ctor(<inner>)`.
fn is_return_wrap_call(e: &Expr, ctor: &str, inner_check: &dyn Fn(&Expr) -> bool) -> bool {
    let Expr::Return(ExprReturn {
        expr: Some(inner), ..
    }) = e
    else {
        return false;
    };
    is_wrap_call(inner, ctor, inner_check)
}

/// `return None`.
fn is_return_none(e: &Expr) -> bool {
    matches!(
        e,
        Expr::Return(ExprReturn { expr: Some(inner), .. }) if is_none_path(inner)
    )
}

/// `<ident>.into()`.
fn is_into_call(e: &Expr, ident: &syn::Ident) -> bool {
    let Expr::MethodCall(mc) = e else {
        return false;
    };
    if mc.method != "into" {
        return false;
    }
    if !mc.args.is_empty() {
        return false;
    }
    is_ident_path(&mc.receiver, ident)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count_in(src: &str) -> usize {
        let file: syn::File = syn::parse_str(src).expect("valid Rust source");
        let suppress = Suppressions::parse(src);
        let mut out = Vec::new();
        let mut v = MatchVisitor {
            rel: "fixture.rs",
            suppress: &suppress,
            out: &mut out,
        };
        v.visit_file(&file);
        out.len()
    }

    #[test]
    fn bubble_flagged() {
        let n = count_in(
            "fn f(r: Result<i32, ()>) -> Result<i32, ()> { let v = match r { Ok(v) => v, Err(e) => return Err(e) }; Ok(v) }",
        );
        assert_eq!(n, 1);
    }

    #[test]
    fn bubble_into_flagged() {
        let n = count_in(
            "fn f(r: Result<i32, A>) -> Result<i32, B> { let v = match r { Ok(v) => v, Err(e) => return Err(e.into()) }; Ok(v) }",
        );
        assert_eq!(n, 1);
    }

    #[test]
    fn wrap_into_flagged() {
        let n = count_in(
            "fn f(r: Result<i32, A>) -> Result<i32, B> { match r { Ok(v) => Ok(v), Err(e) => Err(e.into()) } }",
        );
        assert_eq!(n, 1);
    }

    #[test]
    fn identity_flagged() {
        let n = count_in(
            "fn f(r: Result<i32, ()>) -> Result<i32, ()> { match r { Ok(v) => Ok(v), Err(e) => Err(e) } }",
        );
        assert_eq!(n, 1);
    }

    #[test]
    fn option_bubble_flagged() {
        let n = count_in(
            "fn f(opt: Option<i32>) -> Option<i32> { let v = match opt { Some(v) => v, None => return None }; Some(v) }",
        );
        assert_eq!(n, 1);
    }

    #[test]
    fn option_map_flagged() {
        let n = count_in(
            "fn f(opt: Option<i32>) -> Option<i32> { match opt { Some(v) => Some(v + 1), None => None } }",
        );
        assert_eq!(n, 1);
    }

    #[test]
    fn arm_order_does_not_matter() {
        let n = count_in(
            "fn f(r: Result<i32, ()>) -> Result<i32, ()> { let v = match r { Err(e) => return Err(e), Ok(v) => v }; Ok(v) }",
        );
        assert_eq!(n, 1);
    }

    #[test]
    fn legitimate_transform_not_flagged() {
        // Success arm transforms via outside computation — `?` would change semantics.
        let n = count_in(
            "fn f(r: Result<i32, ()>) -> i32 { match r { Ok(v) => v + 100, Err(_) => 0 } }",
        );
        assert_eq!(n, 0);
    }

    #[test]
    fn three_arms_not_flagged() {
        let n = count_in(
            "fn f(r: Result<i32, ()>) -> i32 { match r { Ok(0) => 1, Ok(v) => v, Err(_) => 0 } }",
        );
        assert_eq!(n, 0);
    }

    #[test]
    fn suppression_works() {
        let src = "fn f(r: Result<i32, ()>) -> Result<i32, ()> {
    // xtask-lint-ignore: manual_question_mark
    let v = match r { Ok(v) => v, Err(e) => return Err(e) };
    Ok(v)
}";
        let n = count_in(src);
        assert_eq!(n, 0);
    }
}

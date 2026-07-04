use std::collections::{HashMap, HashSet};

use anyhow::Result;
use kithara_xtask_core::common::{
    parse::parse_file,
    violation::Violation,
    walker::{compile_globs, matches_any, relative_to, workspace_rs_files_scoped},
};
use syn::{
    Expr, FnArg, ImplItem, Item, ItemFn, ItemImpl, Local, Pat, PatIdent, Stmt, Type, TypePath,
    visit::{self, Visit},
};

use super::{Check, Context};
use crate::idioms::config::NoPassthroughBuilderConfig;

pub(crate) const ID: &str = "no_passthrough_builder";

pub(crate) struct NoPassthroughBuilder;

impl Check for NoPassthroughBuilder {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.no_passthrough_builder;
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
            let structs = collect_named_field_structs(&file.items);
            for item in &file.items {
                match item {
                    Item::Fn(f) => check_fn(cfg, &rel, &structs, f, &mut violations),
                    Item::Impl(impl_block) => {
                        check_impl(cfg, &rel, &structs, impl_block, &mut violations);
                    }
                    _ => {}
                }
            }
        }
        violations.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(violations)
    }
}

/// Set of named-field struct idents declared in the same file with
/// at least `min_field_count` fields. We restrict the check to
/// in-file structs because cross-file inspection requires resolution
/// we don't have at the AST level.
fn collect_named_field_structs(items: &[Item]) -> HashSet<String> {
    let mut out = HashSet::new();
    for item in items {
        if let Item::Struct(s) = item
            && let syn::Fields::Named(_) = &s.fields
        {
            out.insert(s.ident.to_string());
        }
    }
    out
}

fn check_impl(
    cfg: &NoPassthroughBuilderConfig,
    rel: &str,
    structs: &HashSet<String>,
    impl_block: &ItemImpl,
    out: &mut Vec<Violation>,
) {
    for item in &impl_block.items {
        if let ImplItem::Fn(method) = item {
            let synth = ItemFn {
                attrs: method.attrs.clone(),
                vis: method.vis.clone(),
                sig: method.sig.clone(),
                block: Box::new(method.block.clone()),
            };
            check_fn(cfg, rel, structs, &synth, out);
        }
    }
}

fn check_fn(
    cfg: &NoPassthroughBuilderConfig,
    rel: &str,
    structs: &HashSet<String>,
    f: &ItemFn,
    out: &mut Vec<Violation>,
) {
    let Some((param_name, struct_ident)) = single_struct_param(&f.sig.inputs, structs) else {
        return;
    };
    let Some(usage) = analyse_body(&f.block.stmts, &param_name) else {
        return;
    };
    if usage.fields.len() < cfg.min_passthrough_fields {
        return;
    }
    let s = f.sig.fn_token.span.start();
    let key = format!("{}:{}:{}", rel, s.line, s.column);
    let msg = format!(
        "passthrough builder: `fn {}` unpacks `{}` field-by-field ({} fields) without any \
         added semantics. Either construct the target type directly at the call site (one \
         fluent chain) or move meaningful logic into the converter so the intermediate type \
         earns its keep. See xtask/src/idioms/checks/no_passthrough_builder.rs for examples.",
        f.sig.ident,
        struct_ident,
        usage.fields.len()
    );
    out.push(Violation::warn(ID, key, msg));
}

/// Returns (`param_ident`, `type_ident`) when the function takes a single
/// named-field struct parameter (by value or by reference), and that
/// struct is declared in the same file.
fn single_struct_param(
    inputs: &syn::punctuated::Punctuated<FnArg, syn::Token![,]>,
    structs: &HashSet<String>,
) -> Option<(String, String)> {
    let mut typed_inputs = inputs.iter().filter_map(|arg| match arg {
        FnArg::Typed(pt) => Some(pt),
        FnArg::Receiver(_) => None,
    });
    let pt = typed_inputs.next()?;
    if typed_inputs.next().is_some() {
        return None;
    }
    let ident = pat_ident(&pt.pat)?;
    let ty_ident = type_struct_ident(&pt.ty)?;
    if !structs.contains(&ty_ident) {
        return None;
    }
    Some((ident, ty_ident))
}

fn pat_ident(p: &Pat) -> Option<String> {
    match p {
        Pat::Ident(PatIdent { ident, .. }) => Some(ident.to_string()),
        _ => None,
    }
}

fn type_struct_ident(t: &Type) -> Option<String> {
    let path = match t {
        Type::Path(TypePath { path, .. }) => path,
        Type::Reference(r) => return type_struct_ident(&r.elem),
        _ => return None,
    };
    path.segments.last().map(|s| s.ident.to_string())
}

#[derive(Default)]
struct Usage {
    /// Set of field names of `param` that the body uses in a passthrough
    /// shape (direct setter / direct assignment / `if let Some` setter).
    fields: HashSet<String>,
    /// `true` once we see something disqualifying — any expression that
    /// touches `param.X` together with another field, or any control
    /// flow more complex than the recognised passthrough shapes.
    disqualified: bool,
}

fn analyse_body(stmts: &[Stmt], param: &str) -> Option<Usage> {
    let mut usage = Usage::default();
    for stmt in stmts {
        if usage.disqualified {
            return None;
        }
        match stmt {
            Stmt::Local(local) => {
                analyse_local(local, param, &mut usage);
            }
            Stmt::Expr(expr, _) => {
                analyse_expr_stmt(expr, param, &mut usage);
            }
            Stmt::Item(_) => {}
            Stmt::Macro(_) => {
                if stmt_touches_param_anywhere(stmt, param) {
                    usage.disqualified = true;
                }
            }
        }
    }
    if usage.disqualified || !usage.has_only_unique_uses() {
        return None;
    }
    Some(usage)
}

impl Usage {
    /// Each field must be used exactly once across the body. We track
    /// that implicitly: the analysers below insert into `fields` and
    /// flip `disqualified` if they see a duplicate.
    fn record(&mut self, field: &str) {
        if !self.fields.insert(field.to_string()) {
            self.disqualified = true;
        }
    }

    fn has_only_unique_uses(&self) -> bool {
        !self.disqualified
    }
}

/// `let mut cfg = Target::default().with_X(inputs.X).with_Y(inputs.Y);`
/// Counts every `inputs.<field>` in the chain.
fn analyse_local(local: &Local, param: &str, usage: &mut Usage) {
    let Some(init) = &local.init else { return };
    if init.diverge.is_some() {
        usage.disqualified = true;
        return;
    }
    record_passthrough_uses(&init.expr, param, usage);
}

fn analyse_expr_stmt(expr: &Expr, param: &str, usage: &mut Usage) {
    match expr {
        Expr::If(if_expr) => analyse_if(if_expr, param, usage),
        Expr::Assign(assign) => analyse_assign(assign, param, usage),
        Expr::Path(_) => {}
        Expr::MethodCall(_) | Expr::Call(_) => record_passthrough_uses(expr, param, usage),
        _ => {
            if expr_touches_param_anywhere(expr, param) {
                usage.disqualified = true;
            }
        }
    }
}

fn analyse_if(if_expr: &syn::ExprIf, param: &str, usage: &mut Usage) {
    let Expr::Let(let_expr) = &*if_expr.cond else {
        if expr_touches_param_anywhere(&Expr::If(if_expr.clone()), param) {
            usage.disqualified = true;
        }
        return;
    };
    if if_expr.else_branch.is_some() {
        usage.disqualified = true;
        return;
    }
    let Some(field) = expr_param_field(&let_expr.expr, param) else {
        usage.disqualified = true;
        return;
    };
    let Some(bound) = some_pat_ident(&let_expr.pat) else {
        usage.disqualified = true;
        return;
    };
    if !block_is_single_setter_using(&if_expr.then_branch, &bound) {
        usage.disqualified = true;
        return;
    }
    usage.record(&field);
}

fn analyse_assign(assign: &syn::ExprAssign, param: &str, usage: &mut Usage) {
    let Some(field) = expr_param_field(&assign.right, param) else {
        if expr_touches_param_anywhere(&assign.right, param) {
            usage.disqualified = true;
        }
        return;
    };
    usage.record(&field);
}

/// Walks a chained-method or call expression and records every direct
/// `param.field` (or `param.field.clone()`, `param.field.into()`) it
/// finds. Disqualifies if the expression mixes a `param` field with
/// another `param` field in the same call or with a non-trivial
/// adapter.
fn record_passthrough_uses(expr: &Expr, param: &str, usage: &mut Usage) {
    let mut counts: HashMap<String, usize> = HashMap::new();
    if !walk_passthrough(expr, param, &mut counts) {
        usage.disqualified = true;
        return;
    }
    for (field, count) in counts {
        if count > 1 {
            usage.disqualified = true;
            return;
        }
        usage.record(&field);
    }
}

/// Recurse through a chain expression, populating `counts` with each
/// occurrence of `param.<field>`. Returns `false` if the shape is
/// disqualifying (two fields in one call, arithmetic, etc.).
fn walk_passthrough(expr: &Expr, param: &str, counts: &mut HashMap<String, usize>) -> bool {
    match expr {
        Expr::MethodCall(mc) => {
            if !walk_passthrough(&mc.receiver, param, counts) {
                return false;
            }
            let is_trivial = matches!(
                mc.method.to_string().as_str(),
                "clone" | "into" | "to_string" | "to_owned" | "as_ref" | "as_str"
            ) && mc.args.is_empty();
            if is_trivial {
                return true;
            }
            let mut field_args = 0usize;
            for arg in &mc.args {
                if let Some(f) = expr_param_field(arg, param) {
                    field_args += 1;
                    *counts.entry(f).or_default() += 1;
                } else if expr_touches_param_anywhere(arg, param) {
                    return false;
                }
            }
            field_args <= 1
        }
        Expr::Call(call) => {
            if expr_touches_param_anywhere(&call.func, param) {
                return false;
            }
            let mut field_args = 0usize;
            for arg in &call.args {
                if let Some(f) = expr_param_field(arg, param) {
                    field_args += 1;
                    *counts.entry(f).or_default() += 1;
                } else if expr_touches_param_anywhere(arg, param) {
                    return false;
                }
            }
            field_args <= 1
        }
        Expr::Field(field) => {
            if let Some(f) = expr_param_field(expr, param) {
                *counts.entry(f).or_default() += 1;
                return true;
            }
            walk_passthrough(&field.base, param, counts)
        }
        Expr::Path(_) => true,
        Expr::Reference(r) => walk_passthrough(&r.expr, param, counts),
        Expr::Paren(p) => walk_passthrough(&p.expr, param, counts),
        other => !expr_touches_param_anywhere(other, param),
    }
}

/// `Some(<expr>)` where `<expr>` is `param.field` is part of a
/// passthrough; we treat it as a normal `param.field` use here.
/// Returns the field name when `expr` is `param.<field>` or
/// `param.<field>.clone()` / `.into()` / `.as_ref()` etc.
fn expr_param_field(expr: &Expr, param: &str) -> Option<String> {
    match expr {
        Expr::Field(f) => {
            if let Expr::Path(p) = &*f.base
                && p.path.is_ident(param)
                && let syn::Member::Named(name) = &f.member
            {
                return Some(name.to_string());
            }
            None
        }
        Expr::MethodCall(mc) if mc.args.is_empty() => match mc.method.to_string().as_str() {
            "clone" | "into" | "to_string" | "to_owned" | "as_ref" | "as_str" => {
                expr_param_field(&mc.receiver, param)
            }
            _ => None,
        },
        Expr::Call(call) => {
            if call.args.len() != 1 {
                return None;
            }
            let arg = call.args.first()?;
            if let Some(f) = expr_param_field(arg, param) {
                return Some(f);
            }
            if let Expr::Reference(r) = arg
                && let Some(f) = expr_param_field(&r.expr, param)
            {
                return Some(f);
            }
            None
        }
        Expr::Paren(p) => expr_param_field(&p.expr, param),
        Expr::Reference(r) => expr_param_field(&r.expr, param),
        _ => None,
    }
}

/// Recurse and check if any sub-expression accesses `param.<something>`.
/// Used to decide whether an unrecognised shape is innocuous (no
/// `param` use at all) or disqualifying (touches `param` in a way the
/// passthrough analyser does not understand).
fn expr_touches_param_anywhere(expr: &Expr, param: &str) -> bool {
    struct V<'a> {
        param: &'a str,
        hit: bool,
    }
    impl<'ast> Visit<'ast> for V<'_> {
        fn visit_expr_field(&mut self, ef: &'ast syn::ExprField) {
            if let Expr::Path(p) = &*ef.base
                && p.path.is_ident(self.param)
            {
                self.hit = true;
            }
            visit::visit_expr_field(self, ef);
        }
        fn visit_expr_path(&mut self, ep: &'ast syn::ExprPath) {
            if ep.path.is_ident(self.param) {
                self.hit = true;
            }
            visit::visit_expr_path(self, ep);
        }
    }
    let mut v = V { param, hit: false };
    v.visit_expr(expr);
    v.hit
}

fn stmt_touches_param_anywhere(stmt: &Stmt, param: &str) -> bool {
    match stmt {
        Stmt::Macro(m) => m
            .mac
            .tokens
            .clone()
            .into_iter()
            .any(|t| t.to_string() == param),
        Stmt::Expr(e, _) => expr_touches_param_anywhere(e, param),
        Stmt::Local(l) => l
            .init
            .as_ref()
            .is_some_and(|i| expr_touches_param_anywhere(&i.expr, param)),
        Stmt::Item(_) => false,
    }
}

fn some_pat_ident(p: &Pat) -> Option<String> {
    let Pat::TupleStruct(ts) = p else { return None };
    let last = ts.path.segments.last()?;
    if last.ident != "Some" || ts.elems.len() != 1 {
        return None;
    }
    pat_ident(ts.elems.first()?)
}

fn block_is_single_setter_using(b: &syn::Block, bound: &str) -> bool {
    let mut stmts = b.stmts.iter();
    let Some(first) = stmts.next() else {
        return false;
    };
    if stmts.next().is_some() {
        return false;
    }
    let expr = match first {
        Stmt::Expr(e, _) => e,
        Stmt::Local(local) => {
            let Some(init) = &local.init else {
                return false;
            };
            return expr_consumes_only_bound(&init.expr, bound);
        }
        _ => return false,
    };
    expr_consumes_only_bound(expr, bound)
}

/// A single setter: `cfg = cfg.with_X(bound);` or
/// `cfg = cfg.with_X(bound.clone());`. The expression must reference
/// `bound` exactly once and nothing else outside the receiver chain.
fn expr_consumes_only_bound(expr: &Expr, bound: &str) -> bool {
    fn count_ident(e: &Expr, name: &str) -> usize {
        match e {
            Expr::Path(p) => usize::from(p.path.is_ident(name)),
            Expr::MethodCall(mc) => {
                count_ident(&mc.receiver, name)
                    + mc.args.iter().map(|a| count_ident(a, name)).sum::<usize>()
            }
            Expr::Call(c) => {
                count_ident(&c.func, name)
                    + c.args.iter().map(|a| count_ident(a, name)).sum::<usize>()
            }
            Expr::Reference(r) => count_ident(&r.expr, name),
            Expr::Paren(p) => count_ident(&p.expr, name),
            Expr::Assign(a) => count_ident(&a.left, name) + count_ident(&a.right, name),
            _ => 0,
        }
    }
    count_ident(expr, bound) == 1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count_violations(src: &str) -> usize {
        let cfg = NoPassthroughBuilderConfig::default();
        let file: syn::File = syn::parse_str(src).expect("valid Rust file");
        let structs = collect_named_field_structs(&file.items);
        let mut out = Vec::new();
        for item in &file.items {
            match item {
                Item::Fn(f) => check_fn(&cfg, "fixture.rs", &structs, f, &mut out),
                Item::Impl(impl_block) => {
                    check_impl(&cfg, "fixture.rs", &structs, impl_block, &mut out);
                }
                _ => {}
            }
        }
        out.len()
    }

    #[test]
    fn flags_field_by_field_passthrough() {
        let src = r#"
            struct Inputs {
                a: u32,
                b: String,
                c: Option<u64>,
                d: Option<String>,
            }
            fn build(inputs: Inputs) -> Target {
                let mut cfg = Target::default()
                    .with_a(inputs.a)
                    .with_b(inputs.b);
                if let Some(c) = inputs.c {
                    cfg = cfg.with_c(c);
                }
                if let Some(d) = inputs.d {
                    cfg = cfg.with_d(d);
                }
                cfg
            }
        "#;
        assert_eq!(count_violations(src), 1);
    }

    #[test]
    fn does_not_flag_with_real_logic() {
        let src = r#"
            struct Inputs {
                a: u32,
                b: u32,
                c: u32,
                d: u32,
            }
            fn build(inputs: Inputs) -> u32 {
                inputs.a + inputs.b + inputs.c + inputs.d
            }
        "#;
        assert_eq!(count_violations(src), 0);
    }

    #[test]
    fn does_not_flag_below_threshold() {
        let src = r#"
            struct Inputs {
                a: u32,
                b: u32,
                c: u32,
            }
            fn build(inputs: Inputs) -> Target {
                Target::default()
                    .with_a(inputs.a)
                    .with_b(inputs.b)
                    .with_c(inputs.c)
            }
        "#;
        assert_eq!(count_violations(src), 0);
    }

    #[test]
    fn does_not_flag_unknown_struct() {
        let src = r#"
            fn build(inputs: ExternalInputs) -> Target {
                Target::default()
                    .with_a(inputs.a)
                    .with_b(inputs.b)
                    .with_c(inputs.c)
                    .with_d(inputs.d)
            }
        "#;
        assert_eq!(count_violations(src), 0);
    }

    #[test]
    fn flags_associated_method() {
        let src = r#"
            struct Inputs {
                a: u32,
                b: u32,
                c: u32,
                d: u32,
            }
            struct Builder;
            impl Builder {
                fn from_inputs(inputs: Inputs) -> Target {
                    Target::default()
                        .with_a(inputs.a)
                        .with_b(inputs.b)
                        .with_c(inputs.c)
                        .with_d(inputs.d)
                }
            }
        "#;
        assert_eq!(count_violations(src), 1);
    }
}

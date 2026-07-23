use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    path::Path,
};

use anyhow::Result;
use glob::Pattern;
use quote::ToTokens;
use syn::{
    BinOp, Block, Expr, ExprAssign, ExprBinary, ExprCall, ExprMethodCall, ExprStruct, Fields,
    ImplItem, Item, ItemImpl, Member, Pat, Stmt, Type, Visibility, visit::Visit,
};

use crate::common::{
    parse::parse_file,
    suppress::Suppressions,
    walker::{matches_any, relative_to, workspace_rs_files_scoped},
};

#[derive(Debug)]
pub(crate) struct StructInfo {
    pub(crate) rel: String,
    /// Field names in declaration order.
    pub(crate) field_names: Vec<String>,
    /// `true` for bare `pub` (cross-crate API). Such structs are skipped by
    /// most heuristics — external callers may build them in ways the index
    /// can't see.
    pub(crate) is_pub: bool,
    pub(crate) line: usize,
}

#[derive(Debug)]
pub(crate) struct LiteralSite {
    /// `field_name → normalised token string of the assigned expression`.
    /// Includes shorthand (`X { a }` becomes `a → "a"`).
    pub(crate) field_exprs: BTreeMap<String, String>,
    /// Name of the function this literal is an argument to, when the
    /// surrounding expression is a direct call. `None` when the literal is in
    /// a non-argument position (return value, `let` binding, field of another
    /// struct, etc.).
    pub(crate) parent_fn: Option<String>,
    /// `true` when the literal uses `..base` syntax — some fields are not
    /// explicitly listed. `field_exprs` then contains only the fields that
    /// were spelled out.
    pub(crate) has_rest: bool,
}

#[derive(Debug, Default)]
pub(crate) struct WorkspaceStructIndex {
    /// Map struct-name → set of function names whose first statement is
    /// the destructuring pattern `let StructName { ... } = arg;`. Used to
    /// identify single-purpose destructuring consumers.
    pub(crate) destructuring_consumers: BTreeMap<String, BTreeSet<String>>,
    /// Number of `fn` items declared in inherent `impl X` blocks. Excludes
    /// trait impls (so `#[derive]`-generated and hand-written `impl Default`
    /// don't count).
    pub(crate) impl_method_counts: BTreeMap<String, usize>,
    /// Crate name → names of fields used as assignment targets.
    pub(crate) assigned_fields: HashMap<String, HashSet<String>>,
    pub(crate) literals: BTreeMap<String, Vec<LiteralSite>>,
    pub(crate) structs: BTreeMap<String, StructInfo>,
    pub(crate) suppressions: HashMap<String, Suppressions>,
}

pub(crate) fn build_index(
    workspace_root: &Path,
    scope: &crate::common::scope::Scope,
    exempt_globs: &[Pattern],
) -> Result<WorkspaceStructIndex> {
    let mut idx = WorkspaceStructIndex::default();
    for path in workspace_rs_files_scoped(workspace_root, scope)? {
        let rel = relative_to(workspace_root, &path);
        if matches_any(exempt_globs, rel) {
            continue;
        }
        let Ok(file) = parse_file(&path) else {
            continue;
        };
        let src = std::fs::read_to_string(&path)?;
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        idx.suppressions
            .insert(rel_str.clone(), Suppressions::parse(&src));
        collect_assigned_fields(&file, &rel_str, &mut idx);
        collect_in_items(&file.items, &rel_str, &mut idx);
    }
    Ok(idx)
}

fn collect_in_items(items: &[Item], rel: &str, idx: &mut WorkspaceStructIndex) {
    for item in items {
        match item {
            Item::Mod(m) => {
                if let Some((_, inner)) = &m.content {
                    collect_in_items(inner, rel, idx);
                }
            }
            Item::Struct(s) => {
                let Fields::Named(named) = &s.fields else {
                    continue;
                };
                let name = s.ident.to_string();
                let info = StructInfo {
                    rel: rel.to_string(),
                    line: s.ident.span().start().line,
                    field_names: named
                        .named
                        .iter()
                        .filter_map(|f| f.ident.as_ref().map(ToString::to_string))
                        .collect(),
                    is_pub: matches!(s.vis, Visibility::Public(_)),
                };
                idx.structs.insert(name, info);
            }
            Item::Impl(im) => {
                inspect_impl(im, idx);
            }
            Item::Fn(f) => {
                let fn_name = f.sig.ident.to_string();
                inspect_fn(&fn_name, &f.block, idx);
            }
            _ => {}
        }
    }
}

fn inspect_impl(im: &ItemImpl, idx: &mut WorkspaceStructIndex) {
    let inherent = im.trait_.is_none();
    let target = inherent.then(|| self_ty_name(&im.self_ty)).flatten();
    if let Some(name) = target.as_ref() {
        let methods = im
            .items
            .iter()
            .filter(|i| matches!(i, ImplItem::Fn(_)))
            .count();
        *idx.impl_method_counts.entry(name.clone()).or_default() += methods;
    }
    for it in &im.items {
        if let ImplItem::Fn(f) = it {
            let fn_name = f.sig.ident.to_string();
            inspect_fn(&fn_name, &f.block, idx);
        }
    }
}

fn inspect_fn(fn_name: &str, block: &Block, idx: &mut WorkspaceStructIndex) {
    if let Some(Stmt::Local(local)) = block.stmts.first()
        && let Some(struct_name) = leading_destructure_name(&local.pat)
    {
        idx.destructuring_consumers
            .entry(struct_name)
            .or_default()
            .insert(fn_name.to_string());
    }
    let mut visitor = LiteralVisitor { idx };
    visitor.visit_block(block);
}

pub(crate) fn crate_name_from_rel(rel: &str) -> Option<&str> {
    let mut components = rel.split('/');
    if components.next() == Some("crates") {
        components.next()
    } else {
        None
    }
}

fn leading_destructure_name(pat: &Pat) -> Option<String> {
    match pat {
        Pat::Struct(ps) => ps.path.segments.last().map(|s| s.ident.to_string()),
        Pat::Reference(r) => leading_destructure_name(&r.pat),
        _ => None,
    }
}

fn self_ty_name(ty: &Type) -> Option<String> {
    if let Type::Path(p) = ty {
        return p.path.segments.last().map(|s| s.ident.to_string());
    }
    None
}

struct AssignedFieldVisitor<'a> {
    fields: &'a mut HashSet<String>,
}

impl AssignedFieldVisitor<'_> {
    fn record_assignment(&mut self, expr: &Expr) {
        let Expr::Field(field) = expr else {
            return;
        };
        let Member::Named(field_name) = &field.member else {
            return;
        };
        self.fields.insert(field_name.to_string());
    }
}

impl<'ast> Visit<'ast> for AssignedFieldVisitor<'_> {
    fn visit_expr_assign(&mut self, n: &'ast ExprAssign) {
        self.record_assignment(&n.left);
        syn::visit::visit_expr_assign(self, n);
    }

    fn visit_expr_binary(&mut self, n: &'ast ExprBinary) {
        if matches!(
            n.op,
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
        ) {
            self.record_assignment(&n.left);
        }
        syn::visit::visit_expr_binary(self, n);
    }
}

fn collect_assigned_fields(file: &syn::File, rel: &str, idx: &mut WorkspaceStructIndex) {
    let Some(crate_name) = crate_name_from_rel(rel) else {
        return;
    };
    let fields = idx
        .assigned_fields
        .entry(crate_name.to_string())
        .or_default();
    AssignedFieldVisitor { fields }.visit_file(file);
}

struct LiteralVisitor<'a> {
    idx: &'a mut WorkspaceStructIndex,
}

impl LiteralVisitor<'_> {
    fn record_arg(&mut self, es: &ExprStruct, parent_fn: Option<&str>) {
        let Some(name) = es.path.segments.last().map(|s| s.ident.to_string()) else {
            return;
        };
        let mut field_exprs: BTreeMap<String, String> = BTreeMap::new();
        for fv in &es.fields {
            let key = match &fv.member {
                Member::Named(id) => id.to_string(),
                Member::Unnamed(_) => continue,
            };
            field_exprs.insert(key, render_expr(&fv.expr));
        }
        let site = LiteralSite {
            field_exprs,
            parent_fn: parent_fn.map(ToString::to_string),
            has_rest: es.rest.is_some(),
        };
        self.idx.literals.entry(name).or_default().push(site);
    }

    fn visit_call_args(
        &mut self,
        args: &syn::punctuated::Punctuated<Expr, syn::token::Comma>,
        parent_fn: Option<&str>,
    ) {
        for arg in args {
            if let Some(es) = unwrap_argument_struct(arg) {
                self.record_arg(es, parent_fn);
                for f in &es.fields {
                    self.visit_expr(&f.expr);
                }
                if let Some(rest) = &es.rest {
                    self.visit_expr(rest);
                }
            } else {
                self.visit_expr(arg);
            }
        }
    }
}

impl<'ast> Visit<'ast> for LiteralVisitor<'_> {
    fn visit_expr_call(&mut self, n: &'ast ExprCall) {
        let parent = call_path_name(&n.func);
        self.visit_expr(&n.func);
        self.visit_call_args(&n.args, parent.as_deref());
    }

    fn visit_expr_method_call(&mut self, n: &'ast ExprMethodCall) {
        self.visit_expr(&n.receiver);
        let parent = n.method.to_string();
        self.visit_call_args(&n.args, Some(parent.as_str()));
    }

    fn visit_expr_struct(&mut self, n: &'ast ExprStruct) {
        self.record_arg(n, None);
        for f in &n.fields {
            self.visit_expr(&f.expr);
        }
        if let Some(rest) = &n.rest {
            self.visit_expr(rest);
        }
    }
}

fn call_path_name(e: &Expr) -> Option<String> {
    if let Expr::Path(p) = e {
        return p.path.segments.last().map(|s| s.ident.to_string());
    }
    None
}

/// Walk through transparent argument-position wrappers — `&expr`, `&mut expr`,
/// parenthesised `(expr)` — to reach a struct literal underneath. Returns
/// `None` when no such literal is reachable. Constructor wrappers like
/// `Box::new(...)` or `Some(...)` are intentionally NOT unwrapped: those add a
/// real semantic layer (the wrapper is part of the value), so a literal inside
/// is not in raw argument position.
fn unwrap_argument_struct(e: &Expr) -> Option<&ExprStruct> {
    match e {
        Expr::Struct(es) => Some(es),
        Expr::Reference(r) => unwrap_argument_struct(&r.expr),
        Expr::Paren(p) => unwrap_argument_struct(&p.expr),
        _ => None,
    }
}

/// Normalised token-string for syntactic equality. `quote::ToTokens` →
/// whitespace-collapsed string. Two expressions render to the same string iff
/// they're syntactically identical (modulo formatting). Matches what the
/// existing `field_passthrough` check uses for type comparison.
fn render_expr(e: &Expr) -> String {
    e.to_token_stream()
        .to_string()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Convenience used by `field_always_constant` and `field_always_equals_other_field`:
/// returns literal sites that fully describe every field (no `..base`).
pub(crate) fn full_literal_sites(sites: &[LiteralSite]) -> Vec<&LiteralSite> {
    sites.iter().filter(|s| !s.has_rest).collect()
}

/// Helper used by `args_wrapper_struct`: returns `Some(name)` when every
/// literal site is a direct argument to the same function. Otherwise `None`.
pub(crate) fn unique_consumer(sites: &[LiteralSite]) -> Option<&str> {
    let mut single: Option<&str> = None;
    for s in sites {
        let parent = s.parent_fn.as_deref()?;
        match single {
            None => single = Some(parent),
            Some(prev) if prev == parent => {}
            Some(_) => return None,
        }
    }
    single
}

#[cfg(test)]
pub(crate) fn build_index_from_source(src: &str) -> WorkspaceStructIndex {
    const REL: &str = "crates/fixture/src/lib.rs";
    let file: syn::File = syn::parse_str(src).expect("valid Rust source");
    let mut idx = WorkspaceStructIndex::default();
    idx.suppressions
        .insert(REL.to_string(), Suppressions::parse(src));
    collect_assigned_fields(&file, REL, &mut idx);
    collect_in_items(&file.items, REL, &mut idx);
    idx
}

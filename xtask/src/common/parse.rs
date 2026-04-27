//! `syn` parsing helpers shared by checks.

use std::{collections::BTreeMap, path::Path};

use anyhow::Result;
use syn::{
    Block, Expr, File, ImplItem, ImplItemFn, Item, ItemImpl, Member, ReturnType, Signature, Stmt,
    Type,
    visit::{self, Visit},
};

/// Counts of items in a parsed `.rs` file.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct ItemStats {
    pub(crate) types: usize,
    pub(crate) fns: usize,
}

/// Per-type accounting: how much code targets each type defined in the file.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct TypeWeight {
    /// Total `fn`s across all `impl X` and `impl Trait for X` blocks targeting `X`.
    pub(crate) impl_fns: usize,
    /// Number of distinct `impl` blocks targeting this type (own + trait impls).
    pub(crate) impl_blocks: usize,
}

pub(crate) fn parse_file(path: &Path) -> Result<File> {
    let source = std::fs::read_to_string(path)?;
    let file = syn::parse_file(&source)?;
    Ok(file)
}

pub(crate) fn count_items(file: &File) -> ItemStats {
    let mut s = ItemStats::default();
    walk_items(&file.items, &mut s);
    s
}

/// Returns `local_type_name → TypeWeight` for types defined in this file.
/// Impl blocks targeting external types are ignored.
pub(crate) fn type_weights(file: &File) -> BTreeMap<String, TypeWeight> {
    let mut local_types: BTreeMap<String, TypeWeight> = BTreeMap::new();
    collect_local_types(&file.items, &mut local_types);
    accumulate_impls(&file.items, &mut local_types);
    local_types
}

fn collect_local_types(items: &[Item], out: &mut BTreeMap<String, TypeWeight>) {
    for item in items {
        match item {
            Item::Struct(s) => {
                out.entry(s.ident.to_string()).or_default();
            }
            Item::Enum(e) => {
                out.entry(e.ident.to_string()).or_default();
            }
            Item::Trait(t) => {
                out.entry(t.ident.to_string()).or_default();
            }
            Item::Type(t) => {
                out.entry(t.ident.to_string()).or_default();
            }
            Item::Mod(m) => {
                if let Some((_, items)) = &m.content {
                    collect_local_types(items, out);
                }
            }
            _ => {}
        }
    }
}

fn accumulate_impls(items: &[Item], local_types: &mut BTreeMap<String, TypeWeight>) {
    for item in items {
        match item {
            Item::Impl(im) => {
                if let Some(name) = self_ty_name(&im.self_ty)
                    && let Some(w) = local_types.get_mut(&name)
                {
                    w.impl_blocks += 1;
                    for it in &im.items {
                        if let ImplItem::Fn(_) = it {
                            w.impl_fns += 1;
                        }
                    }
                }
            }
            Item::Mod(m) => {
                if let Some((_, items)) = &m.content {
                    accumulate_impls(items, local_types);
                }
            }
            _ => {}
        }
    }
}

pub(crate) fn self_ty_name(ty: &Type) -> Option<String> {
    match ty {
        Type::Path(p) => p.path.segments.last().map(|s| s.ident.to_string()),
        _ => None,
    }
}

/// Stable string for "subject" expressions (paths, field chains, zero-arg
/// method calls, references). Returns `None` for literals/calls/closures —
/// expressions that cannot be a sensible canonical key for grouping or
/// equality (e.g. for matching same-source loops or accumulator targets).
pub(crate) fn canonical_subject(e: &Expr) -> Option<String> {
    match e {
        Expr::Path(p) => Some(
            p.path
                .segments
                .iter()
                .map(|s| s.ident.to_string())
                .collect::<Vec<_>>()
                .join("::"),
        ),
        Expr::Field(fe) => {
            let base = canonical_subject(&fe.base)?;
            let m = match &fe.member {
                Member::Named(n) => n.to_string(),
                Member::Unnamed(i) => i.index.to_string(),
            };
            Some(format!("{base}.{m}"))
        }
        Expr::MethodCall(mc) if mc.args.is_empty() && mc.turbofish.is_none() => {
            let recv = canonical_subject(&mc.receiver)?;
            Some(format!("{recv}.{}()", mc.method))
        }
        Expr::Reference(r) => canonical_subject(&r.expr),
        Expr::Paren(p) => canonical_subject(&p.expr),
        Expr::Group(g) => canonical_subject(&g.expr),
        Expr::Unary(u) => canonical_subject(&u.expr),
        _ => None,
    }
}

/// Whether a method's signature is publicly visible (`pub` or `pub(crate)`).
pub(crate) fn is_pub_visibility(vis: &syn::Visibility) -> bool {
    matches!(
        vis,
        syn::Visibility::Public(_) | syn::Visibility::Restricted(_)
    )
}

/// Strictly-`pub` (not `pub(crate)`).
pub(crate) fn is_strict_pub(vis: &syn::Visibility) -> bool {
    matches!(vis, syn::Visibility::Public(_))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AccessPath {
    pub(crate) kind: AccessKind,
    pub(crate) fields: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AccessKind {
    Ref,
    RefMut,
    Clone,
    Move,
}

#[derive(Debug, Clone)]
pub(crate) struct PassthroughOpts {
    /// `Some`, `Ok`, `Box::new`, `Cow::Borrowed`, ... — single-arg ctors that
    /// wrap a passthrough value and preserve data identity.
    pub(crate) wrapper_ctors: Vec<String>,
    /// `as_ref`, `as_mut`, `as_str`, `as_slice`, `borrow`, `lock`, `read`, ... —
    /// 0-arg methods that expose internal data.
    pub(crate) expose_methods: Vec<String>,
}

impl Default for PassthroughOpts {
    fn default() -> Self {
        Self {
            wrapper_ctors: default_wrapper_ctors(),
            expose_methods: default_expose_methods(),
        }
    }
}

pub(crate) fn default_wrapper_ctors() -> Vec<String> {
    [
        "Some",
        "Ok",
        "Err",
        "Box::new",
        "Arc::new",
        "Rc::new",
        "Pin::new",
        "Pin::new_unchecked",
        "Cow::Borrowed",
        "Cow::Owned",
        "Cell::new",
        "RefCell::new",
        "Mutex::new",
        "RwLock::new",
        "OnceCell::new",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect()
}

pub(crate) fn default_expose_methods() -> Vec<String> {
    [
        "as_ref",
        "as_mut",
        "as_str",
        "as_slice",
        "as_path",
        "as_bytes",
        "as_deref",
        "deref",
        "deref_mut",
        "borrow",
        "borrow_mut",
        "read",
        "write",
        "lock",
        "get",
        "get_mut",
        "into_inner",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect()
}

pub(crate) fn extract_passthrough_with(
    method: &ImplItemFn,
    opts: &PassthroughOpts,
) -> Option<AccessPath> {
    let tail = block_tail_expr(&method.block)?;
    expr_to_passthrough(tail, opts)
}

fn block_tail_expr(b: &Block) -> Option<&Expr> {
    match b.stmts.last()? {
        // tail expression without semicolon
        Stmt::Expr(e, None) => Some(e),
        // explicit `return X;`
        Stmt::Expr(Expr::Return(r), _) => r.expr.as_deref(),
        _ => None,
    }
}

fn expr_to_passthrough(e: &Expr, opts: &PassthroughOpts) -> Option<AccessPath> {
    match e {
        // &self.a.b...
        Expr::Reference(r) => {
            let kind = if r.mutability.is_some() {
                AccessKind::RefMut
            } else {
                AccessKind::Ref
            };
            let fields = self_chain(&r.expr)?;
            Some(AccessPath { kind, fields })
        }
        // self.a.b.<method>() — both `.clone()` and the configurable expose-list
        // (`as_ref`, `lock`, `borrow`, `read`, ...).
        Expr::MethodCall(mc) if mc.args.is_empty() => {
            let receiver_fields = self_chain(&mc.receiver)?;
            let m = mc.method.to_string();
            if m == "clone" {
                Some(AccessPath {
                    kind: AccessKind::Clone,
                    fields: receiver_fields,
                })
            } else if opts.expose_methods.contains(&m) {
                let kind = match m.as_str() {
                    "as_mut" | "borrow_mut" | "deref_mut" | "get_mut" | "write" => {
                        AccessKind::RefMut
                    }
                    _ => AccessKind::Ref,
                };
                Some(AccessPath {
                    kind,
                    fields: receiver_fields,
                })
            } else {
                None
            }
        }
        // Free-function calls of two flavours, single-arg only.
        Expr::Call(call) if call.args.len() == 1 => {
            let callee_path = path_string(call.func.as_ref())?;
            let arg = call.args.first()?;
            // 1) `<X>::clone(&self.field)` — Arc/Rc/Box/T::clone.
            if ends_with_segment(&callee_path, "clone") {
                let Expr::Reference(r) = arg else { return None };
                let fields = self_chain(&r.expr)?;
                return Some(AccessPath {
                    kind: AccessKind::Clone,
                    fields,
                });
            }
            // 2) Wrapper ctor: `Some(...)`, `Box::new(...)`, `Cow::Borrowed(...)`.
            if opts
                .wrapper_ctors
                .iter()
                .any(|w| matches_ctor(&callee_path, w))
            {
                return expr_to_passthrough(arg, opts);
            }
            None
        }
        // `&self.x as &dyn T` — cast preserves the path.
        Expr::Cast(c) => expr_to_passthrough(&c.expr, opts),
        // Direct return of a self-chain (Move).
        Expr::Field(_) | Expr::Path(_) => {
            let fields = self_chain(e)?;
            Some(AccessPath {
                kind: AccessKind::Move,
                fields,
            })
        }
        // Block / explicit return / parens — unwrap and recurse.
        Expr::Block(b) => block_tail_expr(&b.block).and_then(|x| expr_to_passthrough(x, opts)),
        Expr::Return(r) => r.expr.as_deref().and_then(|x| expr_to_passthrough(x, opts)),
        Expr::Paren(p) => expr_to_passthrough(&p.expr, opts),
        _ => None,
    }
}

/// Render an `Expr::Path` callee as `"a::b::c"`. Returns `None` for non-path
/// callees (e.g. closures, parenthesised expressions).
fn path_string(e: &Expr) -> Option<String> {
    let Expr::Path(p) = e else { return None };
    let segs: Vec<String> = p
        .path
        .segments
        .iter()
        .map(|s| s.ident.to_string())
        .collect();
    Some(segs.join("::"))
}

fn ends_with_segment(callee: &str, last: &str) -> bool {
    callee == last || callee.ends_with(&format!("::{last}"))
}

/// Match a wrapper ctor name. Patterns can be plain (`Some`) or path-shaped
/// (`Box::new`); we accept the suffix.
fn matches_ctor(callee: &str, pat: &str) -> bool {
    callee == pat || callee.ends_with(&format!("::{pat}"))
}

fn self_chain(e: &Expr) -> Option<Vec<String>> {
    fn rec(e: &Expr, acc: &mut Vec<String>) -> bool {
        match e {
            Expr::Field(f) => {
                if !rec(&f.base, acc) {
                    return false;
                }
                match &f.member {
                    Member::Named(id) => {
                        acc.push(id.to_string());
                        true
                    }
                    Member::Unnamed(_) => false,
                }
            }
            Expr::Path(p) if p.path.is_ident("self") => true,
            _ => false,
        }
    }
    let mut out = Vec::new();
    if rec(e, &mut out) && !out.is_empty() {
        Some(out)
    } else {
        None
    }
}

/// First field of a `self.a.b.c…` chain (`a`). Returns `None` if expression
/// does not start at `self`.
pub(crate) fn first_self_field(e: &Expr) -> Option<String> {
    let mut path = self_chain(e)?;
    if path.is_empty() {
        return None;
    }
    Some(path.remove(0))
}

/// Detect whether the return type **exposes** an interior-mutability handle.
/// Recognized shapes: `&T`, `&mut T`, `T`, `Option<T>`, `Arc<T>`, `Rc<T>`,
/// `Arc<&T>`, etc., where any path segment in the type tree matches one of
/// `mutable_types` (by last identifier of that segment).
///
/// Used by P3 (mutation handle detection) to flag `pub fn x_handle(&self) ->
/// Arc<AtomicUsize>` style methods, not just `&AtomicUsize` references.
pub(crate) fn returns_handle_type(sig: &Signature, mutable_types: &[String]) -> bool {
    let ReturnType::Type(_, ty) = &sig.output else {
        return false;
    };
    type_exposes_handle(ty, mutable_types)
}

fn type_exposes_handle(ty: &Type, mutable_types: &[String]) -> bool {
    match ty {
        Type::Path(p) => {
            for seg in &p.path.segments {
                if mutable_types.iter().any(|t| seg.ident == *t) {
                    return true;
                }
                if let syn::PathArguments::AngleBracketed(args) = &seg.arguments {
                    for a in &args.args {
                        if let syn::GenericArgument::Type(t) = a
                            && type_exposes_handle(t, mutable_types)
                        {
                            return true;
                        }
                    }
                }
            }
            false
        }
        Type::Reference(r) => type_exposes_handle(&r.elem, mutable_types),
        _ => false,
    }
}

/// Visit a method body and collect names of `self.X` fields that are written
/// to. Recognized writes:
///   - `self.X = ...` (direct assignment),
///   - `self.X.<writer>(...)` for `<writer>` in the configurable list
///     (`store`, `set`, `swap`, `replace`, `fetch_add`, ...).
pub(crate) fn collect_self_field_writes(
    method: &ImplItemFn,
    writer_methods: &[String],
) -> Vec<String> {
    struct WriteVisitor<'a> {
        out: Vec<String>,
        writers: &'a [String],
    }

    impl<'ast> Visit<'ast> for WriteVisitor<'_> {
        fn visit_expr_assign(&mut self, ea: &'ast syn::ExprAssign) {
            if let Some(name) = first_self_field(&ea.left) {
                self.out.push(name);
            }
            visit::visit_expr_assign(self, ea);
        }
        fn visit_expr_method_call(&mut self, mc: &'ast syn::ExprMethodCall) {
            let method = mc.method.to_string();
            if self.writers.contains(&method)
                && let Some(name) = first_self_field(&mc.receiver)
            {
                self.out.push(name);
            }
            visit::visit_expr_method_call(self, mc);
        }
    }

    let mut v = WriteVisitor {
        out: Vec::new(),
        writers: writer_methods,
    };
    v.visit_block(&method.block);
    v.out
}

/// One scope (file root or a nested `mod`) with the items declared **directly**
/// in it. Used by checks that match impls against structs in the same module
/// to avoid cross-module name aliasing.
#[derive(Debug)]
#[expect(
    dead_code,
    reason = "consts/fns consumed by upcoming style::const_locality"
)]
pub(crate) struct Scope<'a> {
    /// Module path components, e.g. `["foo", "bar"]`. Empty for file root.
    pub(crate) path: Vec<String>,
    pub(crate) structs: Vec<&'a syn::ItemStruct>,
    pub(crate) impls: Vec<&'a ItemImpl>,
    pub(crate) consts: Vec<&'a syn::ItemConst>,
    pub(crate) fns: Vec<&'a syn::ItemFn>,
}

/// Recursively collect one `Scope` per module level in the file.
pub(crate) fn collect_scopes(file: &File) -> Vec<Scope<'_>> {
    let mut out = Vec::new();
    collect_scope_inner(&file.items, &mut Vec::new(), &mut out);
    out
}

fn collect_scope_inner<'a>(items: &'a [Item], path: &mut Vec<String>, out: &mut Vec<Scope<'a>>) {
    let mut structs: Vec<&'a syn::ItemStruct> = Vec::new();
    let mut impls: Vec<&'a ItemImpl> = Vec::new();
    let mut consts: Vec<&'a syn::ItemConst> = Vec::new();
    let mut fns: Vec<&'a syn::ItemFn> = Vec::new();
    let mut sub_mods: Vec<(&'a syn::ItemMod, &'a [Item])> = Vec::new();
    for item in items {
        match item {
            Item::Struct(s) => structs.push(s),
            Item::Impl(im) => impls.push(im),
            Item::Const(c) => consts.push(c),
            Item::Fn(f) => fns.push(f),
            Item::Mod(m) => {
                if let Some((_, inner)) = &m.content {
                    sub_mods.push((m, inner));
                }
            }
            _ => {}
        }
    }
    out.push(Scope {
        path: path.clone(),
        structs,
        impls,
        consts,
        fns,
    });
    for (m, inner) in sub_mods {
        path.push(m.ident.to_string());
        collect_scope_inner(inner, path, out);
        path.pop();
    }
}

/// Iterate `pub`/`pub(crate)` methods of an impl block (skipping non-fn items).
pub(crate) fn pub_methods(im: &ItemImpl) -> impl Iterator<Item = &ImplItemFn> {
    im.items.iter().filter_map(|it| match it {
        ImplItem::Fn(f) if is_pub_visibility(&f.vis) => Some(f),
        _ => None,
    })
}

fn walk_items(items: &[Item], s: &mut ItemStats) {
    for item in items {
        match item {
            Item::Struct(_) | Item::Enum(_) | Item::Trait(_) | Item::Type(_) => s.types += 1,
            Item::Fn(_) => s.fns += 1,
            Item::Impl(im) => {
                for it in &im.items {
                    if let ImplItem::Fn(_) = it {
                        s.fns += 1;
                    }
                }
            }
            Item::Mod(m) => {
                if let Some((_, items)) = &m.content {
                    walk_items(items, s);
                }
            }
            _ => {}
        }
    }
}

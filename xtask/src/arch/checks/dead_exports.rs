use std::{
    collections::{BTreeSet, HashSet},
    ops::Range,
    path::{Path, PathBuf},
};

use anyhow::Result;
use cargo_metadata::{Package, Target, TargetKind};
use quote::ToTokens;
use syn::{
    Attribute, Ident, ImplItem, Item, Meta, UseGroup, UsePath, UseTree, Visibility,
    punctuated::Punctuated,
    spanned::Spanned,
    token,
    visit::{self, Visit},
};

use super::{super::config::DeadExportsThreshold, Check, Context};
use crate::common::{
    fix::{FixOutcome, SourceRewriter, expand_blocks},
    parse::{is_pub_visibility, parse_file},
    violation::Violation,
};

pub(crate) const ID: &str = "dead_exports";

pub(crate) struct DeadExports;

impl Check for DeadExports {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.dead_exports;
        let exempt: HashSet<&str> = cfg.exempt.iter().map(String::as_str).collect();
        let (defs, refs) = scan(ctx, cfg);
        let protected = protected_names(&defs, cfg);
        Ok(emit(&defs, &refs, &exempt, &protected))
    }

    fn fix(&self, ctx: &Context<'_>, apply: bool) -> Result<FixOutcome> {
        fix_dead_exports(ctx, apply)
    }
}

/// Walk every non-ignored workspace member: collect strict-`pub` definitions
/// from production-crate `src/`, and identifier references (prod vs test) from
/// everywhere. Shared by `run` (report) and `fix` (delete).
fn scan(ctx: &Context<'_>, cfg: &DeadExportsThreshold) -> (Vec<Def>, Refs) {
    let kinds: HashSet<&str> = cfg.kinds.iter().map(String::as_str).collect();
    let member_ids: HashSet<_> = ctx.metadata.workspace_members.iter().collect();
    let members: Vec<&Package> = ctx
        .metadata
        .packages
        .iter()
        .filter(|p| member_ids.contains(&p.id))
        .collect();

    let mut refs = Refs::default();
    let mut defs: Vec<Def> = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();

    for pkg in &members {
        let role = classify(pkg, cfg);
        if role == Role::Ignored {
            continue;
        }
        for target in targets_sorted(pkg) {
            if target.kind.contains(&TargetKind::CustomBuild) {
                continue;
            }
            let Some(root) = target.src_path.parent() else {
                continue;
            };
            let base_in_test = role == Role::TestOnly || target_is_testish(target);
            for path in walk_rs(root.as_std_path()) {
                if !seen.insert(path.clone()) {
                    continue;
                }
                let Ok(file) = parse_file(&path) else {
                    continue;
                };
                RefCollector {
                    in_test: base_in_test,
                    external: role == Role::TestOnly,
                    refs: &mut refs,
                }
                .visit_file(&file);

                if role == Role::Prod && !base_in_test {
                    let rel = path
                        .strip_prefix(ctx.workspace_root)
                        .unwrap_or(&path)
                        .to_string_lossy()
                        .replace('\\', "/");
                    DefScan {
                        crate_name: &pkg.name,
                        rel: &rel,
                        kinds: &kinds,
                        export_attrs: &cfg.export_attrs,
                        out: &mut defs,
                    }
                    .collect(&file.items, false);
                }
            }
        }
    }
    (defs, refs)
}

fn emit(
    defs: &[Def],
    refs: &Refs,
    exempt: &HashSet<&str>,
    protected: &HashSet<&str>,
) -> Vec<Violation> {
    let mut violations = Vec::new();
    for def in defs {
        if exempt.contains(def.name.as_str())
            || protected.contains(def.name.as_str())
            || refs.prod.contains(&def.name)
        {
            continue;
        }
        let status = if refs.test_external.contains(&def.name) {
            "referenced only by the integration test crate — relocate to tests/ (fixture support)"
        } else if refs.test.contains(&def.name) {
            "referenced only by in-crate #[cfg(test)] — delete the export and its test, or relocate"
        } else {
            "never referenced anywhere — delete"
        };
        let key = format!("{}:{}:{}::{}", def.rel_path, def.line, def.col, def.name);
        let msg = format!(
            "{} `{}` ({}) is exported but {}",
            def.kind, def.name, def.crate_name, status,
        );
        violations.push(Violation::warn(ID, key, msg));
    }
    violations.sort_by(|a, b| a.key.cmp(&b.key));
    violations.dedup_by(|a, b| a.key == b.key);
    violations
}

/// Auto-delete the never-referenced (non-gated) exports and prune the now
/// dangling `pub use` re-exports. Restricted to truly dead names — items with
/// zero references anywhere — so removal cannot break compilation. Cascades
/// (an item that becomes dead once another is gone) are caught by re-running.
/// `cfg(target_os/target_arch)`-gated items are skipped: their callers live in
/// build configurations this scan cannot see.
fn fix_dead_exports(ctx: &Context<'_>, apply: bool) -> Result<FixOutcome> {
    let cfg = &ctx.config.thresholds.dead_exports;
    let (defs, refs) = scan(ctx, cfg);
    let exempt: HashSet<&str> = cfg.exempt.iter().map(String::as_str).collect();
    let protected = protected_names(&defs, cfg);
    let dead: HashSet<String> = defs
        .iter()
        .filter(|d| {
            !refs.prod.contains(&d.name)
                && !refs.test.contains(&d.name)
                && !exempt.contains(d.name.as_str())
                && !protected.contains(d.name.as_str())
        })
        .map(|d| d.name.clone())
        .collect();

    let mut outcome = FixOutcome::default();
    if dead.is_empty() {
        return Ok(outcome);
    }
    let dead_ref: HashSet<&str> = dead.iter().map(String::as_str).collect();

    let member_ids: HashSet<_> = ctx.metadata.workspace_members.iter().collect();
    let members: Vec<&Package> = ctx
        .metadata
        .packages
        .iter()
        .filter(|p| member_ids.contains(&p.id))
        .collect();
    let mut seen: HashSet<PathBuf> = HashSet::new();

    for pkg in &members {
        if classify(pkg, cfg) != Role::Prod {
            continue;
        }
        for target in targets_sorted(pkg) {
            if target.kind.contains(&TargetKind::CustomBuild) || target_is_testish(target) {
                continue;
            }
            let Some(root) = target.src_path.parent() else {
                continue;
            };
            for path in walk_rs(root.as_std_path()) {
                if !seen.insert(path.clone()) {
                    continue;
                }
                let Ok(src) = std::fs::read_to_string(&path) else {
                    continue;
                };
                let Ok(file) = syn::parse_file(&src) else {
                    continue;
                };
                let rel = path
                    .strip_prefix(ctx.workspace_root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/");
                let mut rw = SourceRewriter::new(&src);
                let mut del = Deleter {
                    rel: &rel,
                    dead: &dead_ref,
                    rw: &mut rw,
                    changes: &mut outcome.changes,
                };
                del.scope(&src, 0..src.len(), &file.items);
                del.prune_reexports(&file.items);
                if !rw.is_empty() {
                    if apply {
                        std::fs::write(&path, rw.finish()?)?;
                    }
                    outcome.writes += 1;
                }
            }
        }
    }
    Ok(outcome)
}

struct Deleter<'a, 'src> {
    rel: &'a str,
    dead: &'a HashSet<&'a str>,
    rw: &'a mut SourceRewriter<'src>,
    changes: &'a mut Vec<String>,
}

impl<'src> Deleter<'_, 'src> {
    fn scope(&mut self, src: &'src str, scope_bytes: Range<usize>, items: &[Item]) {
        let spans: Vec<Range<usize>> = items.iter().map(|it| it.span().byte_range()).collect();
        let blocks = expand_blocks(src, scope_bytes, &spans).ok();
        for (i, item) in items.iter().enumerate() {
            match item {
                Item::Mod(m) => {
                    if let Some((brace, inner)) = &m.content {
                        let inner_scope = brace.span.open().byte_range().end
                            ..brace.span.close().byte_range().start;
                        self.scope(src, inner_scope, inner);
                    }
                    continue;
                }
                Item::Impl(im) if im.trait_.is_none() => {
                    self.methods(src, im);
                    continue;
                }
                _ => {}
            }
            if let Some(head) = item_head(item)
                && is_pub_visibility(head.vis)
                && self.dead.contains(head.ident.to_string().as_str())
                && !is_gated(head.attrs)
                && let Some(blocks) = &blocks
            {
                self.rw.replace(blocks[i].bytes.clone(), "");
                self.changes
                    .push(format!("{}: delete `{}`", self.rel, head.ident));
            }
        }
    }

    fn methods(&mut self, src: &'src str, im: &syn::ItemImpl) {
        let scope = im.brace_token.span.open().byte_range().end
            ..im.brace_token.span.close().byte_range().start;
        let spans: Vec<Range<usize>> = im.items.iter().map(|it| it.span().byte_range()).collect();
        let Some(blocks) = expand_blocks(src, scope, &spans).ok() else {
            return;
        };
        for (i, it) in im.items.iter().enumerate() {
            if let ImplItem::Fn(f) = it
                && is_pub_visibility(&f.vis)
                && self.dead.contains(f.sig.ident.to_string().as_str())
                && !is_gated(&f.attrs)
            {
                self.rw.replace(blocks[i].bytes.clone(), "");
                self.changes
                    .push(format!("{}: delete method `{}`", self.rel, f.sig.ident));
            }
        }
    }

    fn prune_reexports(&mut self, items: &[Item]) {
        for item in items {
            match item {
                Item::Use(u) if tree_mentions(&u.tree, self.dead) => {
                    let range = u.span().byte_range();
                    match prune_tree(&u.tree, self.dead) {
                        None => {
                            self.rw.replace(range, "");
                            self.changes
                                .push(format!("{}: drop dangling re-export", self.rel));
                        }
                        Some(pruned) => {
                            let mut nu = u.clone();
                            nu.tree = pruned;
                            self.rw.replace(range, nu.to_token_stream().to_string());
                            self.changes
                                .push(format!("{}: prune dead name from re-export", self.rel));
                        }
                    }
                }
                Item::Mod(m) => {
                    if let Some((_, inner)) = &m.content {
                        self.prune_reexports(inner);
                    }
                }
                _ => {}
            }
        }
    }
}

fn item_head(it: &Item) -> Option<Head<'_>> {
    match it {
        Item::Fn(x) => Some(head(&x.vis, &x.attrs, &x.sig.ident)),
        Item::Const(x) => Some(head(&x.vis, &x.attrs, &x.ident)),
        Item::Static(x) => Some(head(&x.vis, &x.attrs, &x.ident)),
        Item::Struct(x) => Some(head(&x.vis, &x.attrs, &x.ident)),
        Item::Enum(x) => Some(head(&x.vis, &x.attrs, &x.ident)),
        Item::Trait(x) => Some(head(&x.vis, &x.attrs, &x.ident)),
        Item::Type(x) => Some(head(&x.vis, &x.attrs, &x.ident)),
        _ => None,
    }
}

/// Names whose callers this scan cannot see and so must never be reported or
/// auto-deleted: items gated by `#[cfg(target_os/target_arch)]` (compiled on
/// another target) or living under a platform-gated module dir
/// (`fix_protect_paths`, e.g. `/android/`).
fn protected_names<'a>(defs: &'a [Def], cfg: &DeadExportsThreshold) -> HashSet<&'a str> {
    defs.iter()
        .filter(|d| {
            d.gated
                || cfg
                    .fix_protect_paths
                    .iter()
                    .any(|p| d.rel_path.contains(p.as_str()))
        })
        .map(|d| d.name.as_str())
        .collect()
}

/// A `#[cfg(target_os/target_arch = ...)]`-gated item — its callers may live in
/// a build configuration this scan never sees, so deletion is unsafe.
fn is_gated(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|a| {
        a.path().is_ident("cfg")
            && matches!(&a.meta, Meta::List(l)
                if { let t = l.tokens.to_string(); t.contains("target_os") || t.contains("target_arch") })
    })
}

fn tree_mentions(t: &UseTree, dead: &HashSet<&str>) -> bool {
    match t {
        UseTree::Path(p) => tree_mentions(&p.tree, dead),
        UseTree::Name(n) => dead.contains(n.ident.to_string().as_str()),
        UseTree::Rename(r) => dead.contains(r.ident.to_string().as_str()),
        UseTree::Group(g) => g.items.iter().any(|i| tree_mentions(i, dead)),
        UseTree::Glob(_) => false,
    }
}

/// Drop dead leaves from a `use` tree. Returns `None` when nothing survives
/// (the whole `use` should be removed). For a `Rename`, the *source* ident
/// decides — `pub use foo::Bar as Baz` dangles when `Bar` is deleted.
fn prune_tree(t: &UseTree, dead: &HashSet<&str>) -> Option<UseTree> {
    match t {
        UseTree::Path(p) => prune_tree(&p.tree, dead).map(|inner| {
            UseTree::Path(UsePath {
                ident: p.ident.clone(),
                colon2_token: p.colon2_token,
                tree: Box::new(inner),
            })
        }),
        UseTree::Name(n) => (!dead.contains(n.ident.to_string().as_str())).then(|| t.clone()),
        UseTree::Rename(r) => (!dead.contains(r.ident.to_string().as_str())).then(|| t.clone()),
        UseTree::Glob(_) => Some(t.clone()),
        UseTree::Group(g) => {
            let kept: Vec<UseTree> = g.items.iter().filter_map(|i| prune_tree(i, dead)).collect();
            if kept.is_empty() {
                return None;
            }
            let mut group_items: Punctuated<UseTree, token::Comma> = Punctuated::new();
            for k in kept {
                group_items.push(k);
            }
            Some(UseTree::Group(UseGroup {
                brace_token: g.brace_token,
                items: group_items,
            }))
        }
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Role {
    Prod,
    TestOnly,
    Ignored,
}

fn classify(pkg: &Package, cfg: &DeadExportsThreshold) -> Role {
    let name = pkg.name.as_str();
    if cfg.ignore_crates.iter().any(|c| c == name) {
        return Role::Ignored;
    }
    if cfg.test_crates.iter().any(|c| c == name) {
        return Role::TestOnly;
    }
    Role::Prod
}

fn target_is_testish(t: &Target) -> bool {
    t.kind.iter().any(|k| {
        matches!(
            k,
            TargetKind::Test | TargetKind::Bench | TargetKind::Example
        )
    })
}

/// Lib/bin targets before test/bench so a file shared by both is classified as
/// production (the `seen` set keeps the first classification).
fn targets_sorted(pkg: &Package) -> Vec<&Target> {
    let mut ts: Vec<&Target> = pkg.targets.iter().collect();
    ts.sort_by_key(|t| u8::from(target_is_testish(t)));
    ts
}

fn walk_rs(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk_rs_inner(dir, &mut out);
    out.sort();
    out
}

fn walk_rs_inner(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_rs_inner(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

#[derive(Debug)]
struct Def {
    name: String,
    kind: &'static str,
    crate_name: String,
    rel_path: String,
    line: usize,
    col: usize,
    /// `#[cfg(target_os/target_arch)]`-gated at the item: never auto-deleted,
    /// and its re-export is never pruned (a build config this scan can't see
    /// may still use it).
    gated: bool,
}

/// One non-private item head: the three pieces needed to decide whether it is
/// a reportable export and where it lives.
#[derive(Clone, Copy)]
struct Head<'a> {
    vis: &'a Visibility,
    attrs: &'a [Attribute],
    ident: &'a Ident,
}

struct DefScan<'a> {
    crate_name: &'a str,
    rel: &'a str,
    kinds: &'a HashSet<&'a str>,
    export_attrs: &'a [String],
    out: &'a mut Vec<Def>,
}

impl DefScan<'_> {
    fn collect(&mut self, items: &[Item], in_test: bool) {
        for item in items {
            let it_test = in_test || attrs_mark_test(item_attrs(item));
            match item {
                Item::Fn(f) if f.sig.ident != "main" => {
                    self.push(it_test, "fn", head(&f.vis, &f.attrs, &f.sig.ident));
                }
                Item::Const(c) => self.push(it_test, "const", head(&c.vis, &c.attrs, &c.ident)),
                Item::Static(s) => self.push(it_test, "static", head(&s.vis, &s.attrs, &s.ident)),
                Item::Struct(s) => self.push(it_test, "struct", head(&s.vis, &s.attrs, &s.ident)),
                Item::Enum(e) => self.push(it_test, "enum", head(&e.vis, &e.attrs, &e.ident)),
                Item::Trait(t) => self.push(it_test, "trait", head(&t.vis, &t.attrs, &t.ident)),
                Item::Type(t) => self.push(it_test, "type", head(&t.vis, &t.attrs, &t.ident)),
                Item::Impl(im) if im.trait_.is_none() => {
                    if has_export_attr(&im.attrs, self.export_attrs) {
                        continue;
                    }
                    for ii in &im.items {
                        if let ImplItem::Fn(m) = ii {
                            let m_test = it_test || attrs_mark_test(&m.attrs);
                            self.push(m_test, "method", head(&m.vis, &m.attrs, &m.sig.ident));
                        }
                    }
                }
                Item::Mod(m) => {
                    if let Some((_, inner)) = &m.content {
                        self.collect(inner, it_test);
                    }
                }
                _ => {}
            }
        }
    }

    fn push(&mut self, in_test: bool, kind: &'static str, h: Head<'_>) {
        if in_test || !self.kinds.contains(kind) || !is_pub_visibility(h.vis) {
            return;
        }
        if has_export_attr(h.attrs, self.export_attrs) {
            return;
        }
        let start = h.ident.span().start();
        self.out.push(Def {
            name: h.ident.to_string(),
            kind,
            crate_name: self.crate_name.to_string(),
            rel_path: self.rel.to_string(),
            line: start.line,
            col: start.column,
            gated: is_gated(h.attrs),
        });
    }
}

fn head<'a>(vis: &'a Visibility, attrs: &'a [Attribute], ident: &'a Ident) -> Head<'a> {
    Head { vis, attrs, ident }
}

fn item_attrs(it: &Item) -> &[Attribute] {
    match it {
        Item::Fn(x) => &x.attrs,
        Item::Const(x) => &x.attrs,
        Item::Static(x) => &x.attrs,
        Item::Struct(x) => &x.attrs,
        Item::Enum(x) => &x.attrs,
        Item::Trait(x) => &x.attrs,
        Item::Type(x) => &x.attrs,
        Item::Impl(x) => &x.attrs,
        Item::Mod(x) => &x.attrs,
        _ => &[],
    }
}

/// `#[test]`, `#[kithara::test]`/`#[tokio::test]` (last segment `test`), or a
/// `#[cfg(...)]` whose predicate mentions `test`.
fn attrs_mark_test(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|a| {
        let p = a.path();
        if p.segments.last().is_some_and(|s| s.ident == "test") {
            return true;
        }
        if p.is_ident("cfg")
            && let Meta::List(list) = &a.meta
        {
            return list.tokens.to_string().contains("test");
        }
        false
    })
}

fn has_export_attr(attrs: &[Attribute], words: &[String]) -> bool {
    attrs.iter().any(|a| {
        let path_hit = a
            .path()
            .segments
            .iter()
            .any(|seg| words.iter().any(|w| seg.ident == w.as_str()));
        let tok_hit = matches!(&a.meta, Meta::List(l)
            if { let t = l.tokens.to_string(); words.iter().any(|w| t.contains(w.as_str())) });
        path_hit || tok_hit
    })
}

#[derive(Default)]
struct Refs {
    prod: BTreeSet<String>,
    test: BTreeSet<String>,
    /// Subset of `test` whose reference originates in a test-only crate (the
    /// integration `tests/` harness or a `*-test-utils` crate) rather than an
    /// in-crate `#[cfg(test)]` block. These are fixture/harness support — the
    /// relocate-to-`tests/` signal, as opposed to delete-the-in-crate-test.
    test_external: BTreeSet<String>,
}

/// Collects identifier references (path segments + method-call names),
/// bucketed by whether the surrounding context is test code. `use` trees are
/// not `syn::Path` nodes, so re-exports are naturally excluded — only genuine
/// call/type/expression references count. Attribute internals are skipped.
struct RefCollector<'a> {
    in_test: bool,
    external: bool,
    refs: &'a mut Refs,
}

impl RefCollector<'_> {
    fn record(&mut self, name: String) {
        // A `#[kithara::mock]` trait generates `<Name>Mock`; a reference to the
        // generated mock keeps the trait alive even though the names differ.
        // Count the stripped base too (over-counting only ever suppresses a
        // flag, never causes a false deletion).
        if let Some(base) = name.strip_suffix("Mock")
            && !base.is_empty()
        {
            self.insert(base.to_string());
        }
        self.insert(name);
    }

    fn insert(&mut self, name: String) {
        if self.in_test {
            if self.external {
                self.refs.test_external.insert(name.clone());
            }
            self.refs.test.insert(name);
        } else {
            self.refs.prod.insert(name);
        }
    }

    /// Record every identifier inside a token stream (macro body or attribute
    /// args). `syn` does not parse these — `assert_eq!(x.foo())`,
    /// `tracing::info!(bar)`, `#[builder(default = BAZ)]` — so without this a
    /// reference that only appears there is invisible and the symbol looks
    /// dead. Over-recording here is safe: it can only suppress a flag, never
    /// cause a false deletion.
    fn record_tokens(&mut self, ts: &proc_macro2::TokenStream) {
        for tree in ts.clone() {
            match tree {
                proc_macro2::TokenTree::Ident(id) => self.record(id.to_string()),
                proc_macro2::TokenTree::Group(g) => self.record_tokens(&g.stream()),
                _ => {}
            }
        }
    }
}

impl<'ast> Visit<'ast> for RefCollector<'_> {
    fn visit_item_mod(&mut self, m: &'ast syn::ItemMod) {
        let prev = self.in_test;
        self.in_test = prev || attrs_mark_test(&m.attrs);
        visit::visit_item_mod(self, m);
        self.in_test = prev;
    }

    fn visit_item_fn(&mut self, f: &'ast syn::ItemFn) {
        let prev = self.in_test;
        self.in_test = prev || attrs_mark_test(&f.attrs);
        visit::visit_item_fn(self, f);
        self.in_test = prev;
    }

    fn visit_impl_item_fn(&mut self, f: &'ast syn::ImplItemFn) {
        let prev = self.in_test;
        self.in_test = prev || attrs_mark_test(&f.attrs);
        visit::visit_impl_item_fn(self, f);
        self.in_test = prev;
    }

    fn visit_attribute(&mut self, a: &'ast Attribute) {
        if let Meta::List(list) = &a.meta {
            self.record_tokens(&list.tokens);
        }
    }

    fn visit_macro(&mut self, m: &'ast syn::Macro) {
        for seg in &m.path.segments {
            self.record(seg.ident.to_string());
        }
        self.record_tokens(&m.tokens);
    }

    fn visit_path(&mut self, p: &'ast syn::Path) {
        for seg in &p.segments {
            self.record(seg.ident.to_string());
        }
        visit::visit_path(self, p);
    }

    fn visit_expr_method_call(&mut self, mc: &'ast syn::ExprMethodCall) {
        self.record(mc.method.to_string());
        visit::visit_expr_method_call(self, mc);
    }
}

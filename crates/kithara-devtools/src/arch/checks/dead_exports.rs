use std::{
    collections::{BTreeSet, HashMap, HashSet},
    ops::Range,
    path::{Path, PathBuf},
};

use anyhow::Result;
use cargo_metadata::{Package, Target, TargetKind};
use quote::ToTokens;
use syn::{
    Attribute, Expr, Fields, FnArg, Ident, ImplItem, Item, ItemStruct, ItemUse, Local, Member,
    Meta, Pat, Type, UseGroup, UsePath, UseTree, Visibility,
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
    fn fix(&self, ctx: &Context<'_>, apply: bool) -> Result<FixOutcome> {
        fix_dead_exports(ctx, apply)
    }

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
}

/// Walk every workspace member: collect strict-`pub` definitions from
/// production-crate `src/`, and identifier references (prod vs test) from
/// everywhere. Ignored crates contribute only workspace-qualified references.
/// Shared by `run` (report) and `fix` (delete).
fn scan(ctx: &Context<'_>, cfg: &DeadExportsThreshold) -> (Vec<Def>, Refs) {
    let kinds: HashSet<&str> = cfg.kinds.iter().map(String::as_str).collect();
    let member_ids: HashSet<_> = ctx.metadata.workspace_members.iter().collect();
    let members: Vec<&Package> = ctx
        .metadata
        .packages
        .iter()
        .filter(|p| member_ids.contains(&p.id))
        .collect();
    let workspace_crates: HashSet<String> = members
        .iter()
        .map(|pkg| pkg.name.replace('-', "_"))
        .collect();

    let mut refs = Refs::default();
    let mut defs: Vec<Def> = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut chain_cache: HashMap<PathBuf, bool> = HashMap::new();

    for pkg in &members {
        let role = classify(pkg, cfg);
        for target in targets_sorted(pkg) {
            if target.kind.contains(&TargetKind::CustomBuild) {
                continue;
            }
            let Some(root) = target.src_path.parent() else {
                continue;
            };
            let qualified_only = role == Role::Ignored;
            let base_in_test =
                !qualified_only && (role == Role::TestOnly || target_is_testish(target));
            for path in walk_rs(root.as_std_path()) {
                if !seen.insert(path.clone()) {
                    continue;
                }
                let Ok(file) = parse_file(&path) else {
                    continue;
                };
                RefCollector {
                    qualified_only,
                    in_test: base_in_test,
                    external: role == Role::TestOnly,
                    refs: &mut refs,
                    workspace_crates: &workspace_crates,
                    workspace_imports: HashSet::new(),
                    workspace_values: HashSet::new(),
                    workspace_self_fields: HashSet::new(),
                }
                .visit_file(&file);

                if role == Role::Prod && !base_in_test {
                    let rel = path
                        .strip_prefix(ctx.workspace_root)
                        .unwrap_or(&path)
                        .to_string_lossy()
                        .replace('\\', "/");
                    let mod_gated =
                        mod_chain_gated(target.src_path.as_std_path(), &path, &mut chain_cache);
                    DefScan {
                        mod_gated,
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
        violations.push(Violation::deny(ID, key, msg));
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
    dead: &'a HashSet<&'a str>,
    rw: &'a mut SourceRewriter<'src>,
    changes: &'a mut Vec<String>,
    rel: &'a str,
}

impl<'src> Deleter<'_, 'src> {
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
/// auto-deleted: items gated by `#[cfg(target_os/target_arch)]` (at the item
/// or on the `mod`-declaration chain; compiled on another target) or living
/// under a platform-gated module dir (`fix_protect_paths`, e.g. `/android/`).
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

/// A file whose `mod`-declaration chain (crate root down to the file) carries
/// a `#[cfg(target_os/target_arch)]` gate is protected exactly like an
/// item-level cfg: the gate is declared once on the parent `mod x;` (the
/// module-level switch pattern, e.g. `flash/mod.rs` gating `mod api;` /
/// `mod inert;`), so every item inside compiles only in build configurations
/// this scan cannot see.
fn mod_chain_gated(root_file: &Path, file: &Path, cache: &mut HashMap<PathBuf, bool>) -> bool {
    if file == root_file {
        return false;
    }
    if let Some(&gated) = cache.get(file) {
        return gated;
    }
    let gated = decl_gated(root_file, file, cache).unwrap_or(false);
    cache.insert(file.to_path_buf(), gated);
    gated
}

/// Resolve the parent file declaring `file`'s module and combine that
/// declaration's cfg gate with the parent's own chain. `None` when no
/// declaring file is found (unresolvable layouts stay unprotected).
fn decl_gated(root_file: &Path, file: &Path, cache: &mut HashMap<PathBuf, bool>) -> Option<bool> {
    let stem = file.file_stem()?.to_str()?.to_owned();
    let dir = file.parent()?;
    let (name, owner_dir) = if stem == "mod" {
        (dir.file_name()?.to_str()?.to_owned(), dir.parent()?)
    } else {
        (stem, dir)
    };
    for parent in decl_candidates(root_file, owner_dir) {
        let Ok(ast) = parse_file(&parent) else {
            continue;
        };
        if let Some(gated) = find_mod_decl(&ast.items, &name, false) {
            return Some(gated || mod_chain_gated(root_file, &parent, cache));
        }
    }
    None
}

/// Files that may declare a module living in `owner_dir`: the crate root when
/// `owner_dir` is the source root, otherwise `owner_dir/mod.rs` or the 2018
/// sibling `<owner_dir>.rs`.
fn decl_candidates(root_file: &Path, owner_dir: &Path) -> Vec<PathBuf> {
    if Some(owner_dir) == root_file.parent() {
        return vec![root_file.to_path_buf()];
    }
    let mut out = vec![owner_dir.join("mod.rs")];
    if let (Some(parent), Some(dir_name)) = (owner_dir.parent(), owner_dir.file_name()) {
        out.push(parent.join(dir_name).with_extension("rs"));
    }
    out.retain(|p| p.is_file());
    out
}

/// Find the `mod <name>;` declaration (no inline body) among `items`,
/// recursing through inline modules; returns whether the declaration or any
/// enclosing inline module is cfg-gated.
fn find_mod_decl(items: &[Item], name: &str, enclosing: bool) -> Option<bool> {
    for item in items {
        let Item::Mod(m) = item else {
            continue;
        };
        let gated = enclosing || is_gated(&m.attrs);
        match &m.content {
            None if m.ident == name => return Some(gated),
            Some((_, inner)) => {
                if let Some(found) = find_mod_decl(inner, name, gated) {
                    return Some(found);
                }
            }
            None => {}
        }
    }
    None
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
    kind: &'static str,
    crate_name: String,
    name: String,
    rel_path: String,
    /// `#[cfg(target_os/target_arch)]`-gated — at the item or anywhere on the
    /// file's `mod`-declaration chain: never auto-deleted, and its re-export
    /// is never pruned (a build config this scan can't see may still use it).
    gated: bool,
    col: usize,
    line: usize,
}

/// One non-private item head: the three pieces needed to decide whether it is
/// a reportable export and where it lives.
#[derive(Clone, Copy)]
struct Head<'a> {
    ident: &'a Ident,
    vis: &'a Visibility,
    attrs: &'a [Attribute],
}

struct DefScan<'a> {
    kinds: &'a HashSet<&'a str>,
    out: &'a mut Vec<Def>,
    export_attrs: &'a [String],
    crate_name: &'a str,
    rel: &'a str,
    /// The file's `mod`-declaration chain carries a cfg gate; every def in
    /// the file inherits it (see [`mod_chain_gated`]).
    mod_gated: bool,
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
            kind,
            name: h.ident.to_string(),
            crate_name: self.crate_name.to_string(),
            rel_path: self.rel.to_string(),
            line: start.line,
            col: start.column,
            gated: self.mod_gated || is_gated(h.attrs),
        });
    }
}

fn head<'a>(vis: &'a Visibility, attrs: &'a [Attribute], ident: &'a Ident) -> Head<'a> {
    Head { ident, vis, attrs }
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
    workspace_crates: &'a HashSet<String>,
    refs: &'a mut Refs,
    workspace_imports: HashSet<String>,
    workspace_self_fields: HashSet<String>,
    workspace_values: HashSet<String>,
    external: bool,
    in_test: bool,
    qualified_only: bool,
}

impl RefCollector<'_> {
    fn expr_call_returns_workspace_value(&self, func: &Expr) -> bool {
        let Expr::Path(path) = func else {
            return false;
        };
        let Some(first) = path.path.segments.first() else {
            return false;
        };
        path.path.segments.len() > 1
            && self.is_workspace_import(&first.ident)
            && starts_with_uppercase(&first.ident)
    }

    fn expr_field_is_workspace_value(&self, field: &syn::ExprField) -> bool {
        let Expr::Path(base) = field.base.as_ref() else {
            return false;
        };
        let Some(first) = base.path.segments.first() else {
            return false;
        };
        if first.ident != "self" {
            return false;
        }
        let Member::Named(member) = &field.member else {
            return false;
        };
        self.workspace_self_fields
            .iter()
            .any(|field_name| member == field_name.as_str())
    }

    fn expr_is_workspace_value(&self, expr: &Expr) -> bool {
        match expr {
            Expr::Path(path) => {
                let Some(first) = path.path.segments.first() else {
                    return false;
                };
                self.is_workspace_value(&first.ident)
                    || self.is_workspace_crate(&first.ident)
                    || (self.is_workspace_import(&first.ident) && path.path.segments.len() > 1)
            }
            Expr::Field(field) => self.expr_field_is_workspace_value(field),
            Expr::Call(call) => self.expr_call_returns_workspace_value(&call.func),
            Expr::Try(expr_try) => self.expr_is_workspace_value(&expr_try.expr),
            Expr::Group(group) => self.expr_is_workspace_value(&group.expr),
            Expr::Paren(paren) => self.expr_is_workspace_value(&paren.expr),
            Expr::Reference(reference) => self.expr_is_workspace_value(&reference.expr),
            _ => false,
        }
    }

    fn expr_returns_workspace_value(&self, expr: &Expr) -> bool {
        match expr {
            Expr::Call(call) => self.expr_call_returns_workspace_value(&call.func),
            Expr::Try(expr_try) => self.expr_returns_workspace_value(&expr_try.expr),
            Expr::Group(group) => self.expr_returns_workspace_value(&group.expr),
            Expr::Paren(paren) => self.expr_returns_workspace_value(&paren.expr),
            Expr::Reference(reference) => self.expr_returns_workspace_value(&reference.expr),
            _ => self.expr_is_workspace_value(expr),
        }
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

    fn is_workspace_crate(&self, ident: &Ident) -> bool {
        self.workspace_crates
            .iter()
            .any(|crate_name| ident == crate_name.as_str())
    }

    fn is_workspace_import(&self, ident: &Ident) -> bool {
        self.workspace_imports
            .iter()
            .any(|import| ident == import.as_str())
    }

    fn is_workspace_value(&self, ident: &Ident) -> bool {
        self.workspace_values
            .iter()
            .any(|value| ident == value.as_str())
    }

    fn path_is_workspace_rooted(&self, path: &syn::Path) -> bool {
        path.segments.first().is_some_and(|first| {
            self.is_workspace_crate(&first.ident) || self.is_workspace_import(&first.ident)
        })
    }

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

    fn record_qualified_path(&mut self, path: &syn::Path) {
        let Some(first) = path.segments.first() else {
            return;
        };
        if self.qualified_only
            && !self.is_workspace_crate(&first.ident)
            && (!self.is_workspace_import(&first.ident) || path.segments.len() == 1)
        {
            return;
        }
        for seg in &path.segments {
            self.record(seg.ident.to_string());
        }
    }

    fn record_qualified_tokens(&mut self, ts: &proc_macro2::TokenStream) {
        let tokens: Vec<proc_macro2::TokenTree> = ts.clone().into_iter().collect();
        let mut idx = 0;
        while idx < tokens.len() {
            match &tokens[idx] {
                proc_macro2::TokenTree::Ident(ident)
                    if self.is_workspace_crate(ident) || self.is_workspace_import(ident) =>
                {
                    if let Some((next, names)) = qualified_token_path(&tokens, idx) {
                        for name in names {
                            self.record(name);
                        }
                        idx = next;
                        continue;
                    }
                }
                proc_macro2::TokenTree::Group(group) => {
                    self.record_qualified_tokens(&group.stream());
                }
                _ => {}
            }
            idx += 1;
        }
    }

    fn record_qualified_use_tree(&mut self, tree: &UseTree, rooted: bool) {
        match tree {
            UseTree::Path(path) => {
                let next_rooted = rooted || self.is_workspace_crate(&path.ident);
                if next_rooted && use_tree_imports_self(&path.tree) {
                    self.workspace_imports.insert(path.ident.to_string());
                }
                if next_rooted {
                    self.record_qualified_use_tree(&path.tree, next_rooted);
                }
            }
            UseTree::Name(name) if rooted => {
                self.record(name.ident.to_string());
                if name.ident != "self" {
                    self.workspace_imports.insert(name.ident.to_string());
                }
            }
            UseTree::Rename(rename) if rooted => {
                self.record(rename.ident.to_string());
                self.workspace_imports.insert(rename.rename.to_string());
            }
            UseTree::Group(group) => {
                for item in &group.items {
                    self.record_qualified_use_tree(item, rooted);
                }
            }
            UseTree::Glob(_) | UseTree::Name(_) | UseTree::Rename(_) => {}
        }
    }

    /// Record every identifier inside a token stream (macro body or attribute
    /// args). `syn` does not parse these — `assert_eq!(x.foo())`,
    /// `tracing::info!(bar)`, `#[builder(default = BAZ)]` — so without this a
    /// reference that only appears there is invisible and the symbol looks
    /// dead. Over-recording here is safe: it can only suppress a flag, never
    /// cause a false deletion.
    fn record_tokens(&mut self, ts: &proc_macro2::TokenStream) {
        if self.qualified_only {
            return;
        }
        for tree in ts.clone() {
            match tree {
                proc_macro2::TokenTree::Ident(id) => self.record(id.to_string()),
                proc_macro2::TokenTree::Group(g) => self.record_tokens(&g.stream()),
                _ => {}
            }
        }
    }

    fn record_workspace_fields(&mut self, fields: &Fields) {
        for field in fields {
            if self.type_is_workspace(&field.ty)
                && let Some(ident) = &field.ident
            {
                self.workspace_self_fields.insert(ident.to_string());
            }
        }
    }

    fn record_workspace_inputs(&mut self, inputs: &Punctuated<FnArg, token::Comma>) {
        for input in inputs {
            let FnArg::Typed(arg) = input else {
                continue;
            };
            if !self.type_is_workspace(&arg.ty) {
                continue;
            }
            if let Some(ident) = pat_ident(&arg.pat) {
                self.workspace_values.insert(ident.to_string());
            }
        }
    }

    fn record_workspace_local(&mut self, local: &Local) {
        let typed_workspace = pat_type(&local.pat).is_some_and(|ty| self.type_is_workspace(ty));
        let init_workspace = local
            .init
            .as_ref()
            .is_some_and(|init| self.expr_returns_workspace_value(&init.expr));
        if !typed_workspace && !init_workspace {
            return;
        }
        if let Some(ident) = pat_ident(&local.pat) {
            self.workspace_values.insert(ident.to_string());
        }
    }

    fn type_is_workspace(&self, ty: &Type) -> bool {
        match ty {
            Type::Path(path) => self.path_is_workspace_rooted(&path.path),
            Type::Reference(reference) => self.type_is_workspace(&reference.elem),
            Type::Group(group) => self.type_is_workspace(&group.elem),
            Type::Paren(paren) => self.type_is_workspace(&paren.elem),
            _ => false,
        }
    }
}

fn qualified_token_path(
    tokens: &[proc_macro2::TokenTree],
    start: usize,
) -> Option<(usize, Vec<String>)> {
    let proc_macro2::TokenTree::Ident(root) = &tokens[start] else {
        return None;
    };
    let mut names = vec![root.to_string()];
    let mut idx = start;
    while token_path_sep_at(tokens, idx + 1) {
        let next_ident = idx + 3;
        let Some(proc_macro2::TokenTree::Ident(ident)) = tokens.get(next_ident) else {
            break;
        };
        names.push(ident.to_string());
        idx = next_ident;
    }
    (names.len() > 1).then_some((idx + 1, names))
}

fn token_path_sep_at(tokens: &[proc_macro2::TokenTree], idx: usize) -> bool {
    let Some(proc_macro2::TokenTree::Punct(first)) = tokens.get(idx) else {
        return false;
    };
    let Some(proc_macro2::TokenTree::Punct(second)) = tokens.get(idx + 1) else {
        return false;
    };
    first.as_char() == ':' && second.as_char() == ':'
}

fn use_tree_imports_self(tree: &UseTree) -> bool {
    match tree {
        UseTree::Path(path) => use_tree_imports_self(&path.tree),
        UseTree::Name(name) => name.ident == "self",
        UseTree::Rename(rename) => rename.ident == "self",
        UseTree::Group(group) => group.items.iter().any(use_tree_imports_self),
        UseTree::Glob(_) => false,
    }
}

fn pat_ident(pat: &Pat) -> Option<&Ident> {
    match pat {
        Pat::Ident(ident) => Some(&ident.ident),
        Pat::Reference(reference) => pat_ident(reference.pat.as_ref()),
        Pat::Type(pat_type) => pat_ident(pat_type.pat.as_ref()),
        _ => None,
    }
}

fn pat_type(pat: &Pat) -> Option<&Type> {
    match pat {
        Pat::Reference(reference) => pat_type(reference.pat.as_ref()),
        Pat::Type(pat_type) => Some(pat_type.ty.as_ref()),
        _ => None,
    }
}

fn starts_with_uppercase(ident: &Ident) -> bool {
    ident
        .to_string()
        .chars()
        .next()
        .is_some_and(char::is_uppercase)
}

impl<'ast> Visit<'ast> for RefCollector<'_> {
    fn visit_attribute(&mut self, a: &'ast Attribute) {
        if self.qualified_only {
            return;
        }
        if let Meta::List(list) = &a.meta {
            self.record_tokens(&list.tokens);
        }
    }

    fn visit_expr_method_call(&mut self, mc: &'ast syn::ExprMethodCall) {
        if self.qualified_only {
            if self.expr_is_workspace_value(&mc.receiver) {
                self.record(mc.method.to_string());
            }
        } else {
            self.record(mc.method.to_string());
        }
        visit::visit_expr_method_call(self, mc);
    }

    fn visit_impl_item_fn(&mut self, f: &'ast syn::ImplItemFn) {
        let prev_in_test = self.in_test;
        let prev_values = self.workspace_values.clone();
        self.in_test = prev_in_test || attrs_mark_test(&f.attrs);
        if self.qualified_only {
            self.record_workspace_inputs(&f.sig.inputs);
        }
        visit::visit_impl_item_fn(self, f);
        self.workspace_values = prev_values;
        self.in_test = prev_in_test;
    }

    fn visit_item_fn(&mut self, f: &'ast syn::ItemFn) {
        let prev_in_test = self.in_test;
        let prev_values = self.workspace_values.clone();
        self.in_test = prev_in_test || attrs_mark_test(&f.attrs);
        if self.qualified_only {
            self.record_workspace_inputs(&f.sig.inputs);
        }
        visit::visit_item_fn(self, f);
        self.workspace_values = prev_values;
        self.in_test = prev_in_test;
    }

    fn visit_item_mod(&mut self, m: &'ast syn::ItemMod) {
        let prev = self.in_test;
        self.in_test = prev || attrs_mark_test(&m.attrs);
        visit::visit_item_mod(self, m);
        self.in_test = prev;
    }

    fn visit_item_struct(&mut self, item_struct: &'ast ItemStruct) {
        if self.qualified_only {
            self.record_workspace_fields(&item_struct.fields);
        }
        visit::visit_item_struct(self, item_struct);
    }

    fn visit_item_use(&mut self, item_use: &'ast ItemUse) {
        if self.qualified_only {
            self.record_qualified_use_tree(&item_use.tree, false);
        } else {
            visit::visit_item_use(self, item_use);
        }
    }

    fn visit_local(&mut self, local: &'ast Local) {
        if self.qualified_only {
            self.record_workspace_local(local);
        }
        visit::visit_local(self, local);
    }

    fn visit_macro(&mut self, m: &'ast syn::Macro) {
        if self.qualified_only {
            self.record_qualified_tokens(&m.tokens);
            return;
        }
        for seg in &m.path.segments {
            self.record(seg.ident.to_string());
        }
        self.record_tokens(&m.tokens);
    }

    fn visit_path(&mut self, p: &'ast syn::Path) {
        self.record_qualified_path(p);
        visit::visit_path(self, p);
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{HashMap, HashSet},
        fs,
        path::Path,
    };

    use syn::visit::Visit;

    use super::{RefCollector, Refs, mod_chain_gated};

    const GATE: &str = "#[cfg(all(not(target_arch = \"wasm32\"), feature = \"flash\"))]";

    fn collect_refs(src: &str, qualified_only: bool) -> Refs {
        let file = syn::parse_file(src).unwrap();
        let workspace_crates: HashSet<String> = ["kithara_devtools"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        let mut refs = Refs::default();
        RefCollector {
            qualified_only,
            in_test: false,
            external: false,
            refs: &mut refs,
            workspace_crates: &workspace_crates,
            workspace_imports: HashSet::new(),
            workspace_values: HashSet::new(),
            workspace_self_fields: HashSet::new(),
        }
        .visit_file(&file);
        refs
    }

    fn write(root: &Path, rel: &str, content: &str) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn gated(root: &Path, rel: &str) -> bool {
        let mut cache = HashMap::new();
        mod_chain_gated(&root.join("lib.rs"), &root.join(rel), &mut cache)
    }

    #[test]
    fn qualified_only_records_workspace_qualified_inline_path() {
        let refs = collect_refs(
            r#"
            fn run() {
                kithara_devtools::util::check_tool();
            }
            "#,
            true,
        );

        assert!(refs.prod.contains("check_tool"));
    }

    #[test]
    fn qualified_only_records_workspace_qualified_use_leaf() {
        let refs = collect_refs(
            r#"
            use kithara_devtools::util::{check_tool as ct, project_name};

            fn run() {
                ct();
                project_name();
            }
            "#,
            true,
        );

        assert!(refs.prod.contains("check_tool"));
        assert!(refs.prod.contains("project_name"));
        assert!(
            !refs.prod.contains("ct"),
            "a renamed import credits the source name, not the local alias"
        );
    }

    #[test]
    fn qualified_only_ignores_unqualified_names_methods_macros_and_attrs() {
        let refs = collect_refs(
            r#"
            #[kithara_devtools::marker(check_tool)]
            fn run() {
                check_tool();
                target.check_tool();
                crate::some_macro!(check_tool);
            }
            "#,
            true,
        );

        assert!(!refs.prod.contains("check_tool"));
        assert!(!refs.prod.contains("some_macro"));
    }

    #[test]
    fn qualified_only_records_methods_on_workspace_imported_values() {
        let refs = collect_refs(
            r#"
            use kithara_devtools::common::{
                baseline::Baseline,
                report,
                scope::Scope,
            };

            fn run(scope: &Scope) {
                let baseline = Baseline::from_report(&report);
                let saved = baseline.save();
                let scoped = scope.key_in_scope("x");
                print!("{}", report::render_json(saved, scoped));
            }
            "#,
            true,
        );

        assert!(refs.prod.contains("from_report"));
        assert!(refs.prod.contains("save"));
        assert!(refs.prod.contains("key_in_scope"));
        assert!(refs.prod.contains("render_json"));
    }

    #[test]
    fn normal_collection_keeps_existing_reference_sources() {
        let refs = collect_refs(
            r#"
            use kithara_devtools::util::imported_leaf;

            #[some_attr(attr_token)]
            fn run() {
                kithara_devtools::util::inline_path();
                receiver.method_name();
                tracing::info!(macro_token);
            }
            "#,
            false,
        );

        assert!(refs.prod.contains("inline_path"));
        assert!(refs.prod.contains("method_name"));
        assert!(refs.prod.contains("macro_token"));
        assert!(refs.prod.contains("attr_token"));
        assert!(
            !refs.prod.contains("imported_leaf"),
            "normal mode must not start counting use-tree leaves"
        );
    }

    #[test]
    fn item_in_cfg_gated_mod_declaration_is_gated() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "lib.rs", "mod flash;");
        write(root, "flash/mod.rs", &format!("{GATE}\nmod api;"));
        write(root, "flash/api.rs", "pub fn dump_to_stderr() {}");
        assert!(
            gated(root, "flash/api.rs"),
            "cfg gate on the parent `mod api;` declaration must protect the file's items"
        );
    }

    #[test]
    fn ungated_mod_chain_stays_unprotected() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "lib.rs", "mod flash;");
        write(root, "flash/mod.rs", "mod api;");
        write(root, "flash/api.rs", "pub fn open() {}");
        assert!(!gated(root, "flash/api.rs"));
    }

    #[test]
    fn gate_on_ancestor_declaration_propagates_down() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "lib.rs", &format!("{GATE}\nmod flash;"));
        write(root, "flash/mod.rs", "mod api;");
        write(root, "flash/api.rs", "pub fn open() {}");
        assert!(
            gated(root, "flash/api.rs"),
            "a gate on `mod flash;` at the crate root must protect nested files"
        );
    }

    #[test]
    fn sibling_file_module_owner_resolves() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "lib.rs", "mod flash;");
        write(root, "flash.rs", &format!("{GATE}\nmod api;"));
        write(root, "flash/api.rs", "pub fn open() {}");
        assert!(
            gated(root, "flash/api.rs"),
            "2018 layout: `flash.rs` owning `flash/api.rs` must be resolved"
        );
    }

    #[test]
    fn crate_root_file_is_never_gated() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "lib.rs", "pub fn open() {}");
        assert!(!gated(root, "lib.rs"));
    }

    #[test]
    fn feature_only_gate_does_not_protect() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "lib.rs", "mod flash;");
        write(
            root,
            "flash/mod.rs",
            "#[cfg(feature = \"flash\")]\nmod api;",
        );
        write(root, "flash/api.rs", "pub fn open() {}");
        assert!(
            !gated(root, "flash/api.rs"),
            "same semantics as item-level cfg: only target_os/target_arch gates protect"
        );
    }
}

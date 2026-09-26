use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    ops::Range,
    path::Path as FsPath,
};

use anyhow::{Context as _, Result};
use proc_macro2::{Spacing, TokenStream, TokenTree};
use quote::ToTokens;
use syn::{
    Arm, Expr, Field, ImplItem, Item, ItemMod, ItemUse, Macro, Path, Stmt, TraitItem, UseTree,
    Variant,
    spanned::Spanned,
    visit::{self, Visit},
};

use super::{Check, Context};
use crate::{
    common::{
        fix::{FixOutcome, SourceRewriter},
        violation::Violation,
        walker::{compile_globs, matches_any, relative_to},
    },
    style::config::QualifiedPathDepthConfig,
};

pub(crate) mod consts {
    pub(crate) const ID: &str = "qualified_path_depth";
}

pub(crate) struct QualifiedPathDepth;

impl Check for QualifiedPathDepth {
    fn fix(&self, ctx: &Context<'_>) -> Result<FixOutcome> {
        let cfg = &ctx.config.thresholds.qualified_path_depth;
        let mut outcome = FixOutcome::default();
        for path in ctx.scan.rs_files(ctx.scope)?.iter() {
            let rel = relative(ctx.workspace_root, path);
            if !cfg.covers(&rel) {
                continue;
            }
            let Some(src) = ctx.scan.source(path) else {
                continue;
            };
            let rewrite = rewrite(cfg, &src)
                .with_context(|| format!("{ID} fix failed for {rel}", ID = consts::ID))?;
            for reason in rewrite.skipped {
                outcome.skipped.push(format!("{rel}: {reason}"));
            }
            let Some(rewritten) = rewrite.source else {
                continue;
            };
            ctx.scan.write(path, rewritten)?;
            outcome.writes += 1;
            outcome.changes.push(rel);
        }
        Ok(outcome)
    }

    fn id(&self) -> &'static str {
        consts::ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.qualified_path_depth;
        let mut violations = Vec::new();
        for path in ctx.scan.rs_files(ctx.scope)?.iter() {
            let rel = relative(ctx.workspace_root, path);
            if !cfg.covers(&rel) {
                continue;
            }
            let Ok(file) = ctx.scan.parse_file(path) else {
                continue;
            };
            let mut visitor = PathVisitor::default();
            visitor.visit_file(&file);
            for found in visitor.found {
                if found.gated || !is_deep(cfg, found.path) {
                    continue;
                }
                let printed = print_path(found.path);
                let line = found.path.segments[0].ident.span().start().line;
                violations.push(Violation::warn(
                    consts::ID,
                    format!("{rel}:{line}::{printed}"),
                    format!(
                        "qualified path `{printed}` is {} segments deep; \
                         `use` the item and name it directly",
                        found.path.segments.len()
                    ),
                ));
            }
        }
        violations.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(violations)
    }
}

fn relative(root: &FsPath, path: &FsPath) -> String {
    relative_to(root, path).to_string_lossy().replace('\\', "/")
}

/// What one file becomes, and the qualifications it keeps.
#[derive(Default)]
struct Rewrite {
    source: Option<String>,
    skipped: Vec<String>,
}

/// Trade a file's deep qualifications for imports.
fn rewrite(cfg: &QualifiedPathDepthConfig, src: &str) -> Result<Rewrite> {
    let Ok(file) = syn::parse_file(src) else {
        return Ok(Rewrite::default());
    };
    let plan = plan_file(cfg, &file);
    if plan.cuts.is_empty() {
        return Ok(Rewrite {
            source: None,
            skipped: plan.skipped,
        });
    }
    let mut rw = SourceRewriter::new(src);
    for cut in &plan.cuts {
        rw.replace(cut.clone(), "");
    }
    for (leaf, text) in plan.leaves {
        rw.replace(leaf, text);
    }
    if !plan.fresh.is_empty() {
        let (at, text) = if let Some(end) = last_use_end(&file) {
            (
                end,
                plan.fresh
                    .iter()
                    .map(|import| format!("\nuse {import};"))
                    .collect(),
            )
        } else {
            let start = first_item_start(&file)
                .context("no `use` and no item to hang the new import on")?;
            let mut text: String = plan
                .fresh
                .iter()
                .map(|import| format!("use {import};\n"))
                .collect();
            text.push('\n');
            (start, text)
        };
        rw.replace(at..at, text);
    }
    Ok(Rewrite {
        source: Some(rw.finish()?),
        skipped: plan.skipped,
    })
}

/// One qualification the file trades for an import.
struct Import {
    /// The name the shortened paths read from.
    binds: String,
    /// The item's full path, as the new `use` spells it.
    full: String,
}

/// Every edit one file takes.
#[derive(Default)]
struct FilePlan {
    /// Imports no leaf could carry, added as their own lines.
    fresh: BTreeSet<String>,
    cuts: Vec<Range<usize>>,
    /// A `use` leaf, and what it reads once it carries the import the
    /// shortened paths used to travel through it for.
    leaves: Vec<(Range<usize>, String)>,
    skipped: Vec<String>,
}

/// What the whole file can be shortened to in one pass.
///
/// The plan is per file because the conflicts are: two paths that would bind
/// the same name to different items cancel each other, and a name the file
/// already spells for something else cancels its path alone. Every path the
/// surviving imports name is then cut, however shallow it was and wherever it
/// stands, because an import that leaves a spelled-out path behind trades a
/// deep path for an `unused_qualifications` warning.
fn plan_file(cfg: &QualifiedPathDepthConfig, file: &syn::File) -> FilePlan {
    let mut plan = FilePlan::default();
    let scope = Scope::of_items(&file.items, "self");
    let mut visitor = PathVisitor::default();
    visitor.visit_file(file);
    if scope.glob.foreign || scope.glob.inherited {
        plan.skipped
            .push("a glob import leaves the file's names unknowable".to_owned());
        return plan;
    }
    if visitor.local_use {
        plan.skipped
            .push("a `use` inside a body renames what a path means".to_owned());
        return plan;
    }

    let mut proposed: Vec<Import> = Vec::new();
    for found in &visitor.found {
        if found.gated || !is_deep(cfg, found.path) {
            continue;
        }
        match propose(&scope, found.path) {
            Ok(import) => proposed.push(import),
            Err(reason) => plan
                .skipped
                .push(format!("{}: {reason}", print_path(found.path))),
        }
    }
    let mut imports = settle(proposed, &mut plan.skipped);
    for _ in 0..imports.len().min(4) {
        let pass = walk(&imports, &file.items, &scope);
        if pass.suspect.is_empty() {
            return finished(plan, pass, &imports, &scope);
        }
        for binds in &pass.suspect {
            plan.skipped.push(format!(
                "`{binds}` is spelled another way where this import would reach"
            ));
        }
        imports.retain(|_, import| !pass.suspect.contains(&import.binds));
        if imports.is_empty() {
            return plan;
        }
    }
    plan
}

/// Walk the file once with one set of imports.
fn walk<'a>(imports: &'a BTreeMap<String, Import>, items: &[Item], scope: &Scope) -> Pass<'a> {
    let mut pass = Pass {
        imports,
        by_name: imports
            .values()
            .map(|import| (import.binds.as_str(), import.full.as_str()))
            .collect(),
        cuts: Vec::new(),
        leaves: Vec::new(),
        carried: HashSet::new(),
        suspect: HashSet::new(),
    };
    let mut stack = vec![Level::new(scope.clone(), false)];
    pass.items(items, &mut stack, true);
    if let Some(top) = stack.pop() {
        pass.settle_level(&top, true);
    }
    pass
}

/// The plan a settled walk comes to.
fn finished(
    mut plan: FilePlan,
    pass: Pass<'_>,
    imports: &BTreeMap<String, Import>,
    scope: &Scope,
) -> FilePlan {
    let mut cuts = pass.cuts;
    cuts.sort_by_key(|cut| cut.start);
    cuts.dedup_by(|later, earlier| earlier.end > later.start);
    plan.cuts = cuts;
    plan.leaves = pass.leaves;
    for (full, import) in imports {
        let spelled = scope
            .bindings
            .get(&import.binds)
            .is_some_and(|binding| &binding.full == full);
        if !pass.carried.contains(full) && !spelled {
            plan.fresh.insert(full.clone());
        }
    }
    plan
}

/// The imports that survive each other, keyed by the item each one names.
///
/// Two paths that would answer to one name in one file cancel: the file would
/// read the same word for two items.
fn settle(proposed: Vec<Import>, skipped: &mut Vec<String>) -> BTreeMap<String, Import> {
    let mut bound: HashMap<&str, &str> = HashMap::new();
    let mut conflicted: HashSet<String> = HashSet::new();
    for import in &proposed {
        match bound.get(import.binds.as_str()) {
            Some(full) if *full != import.full => {
                conflicted.insert(import.binds.clone());
            }
            Some(_) => {}
            None => {
                bound.insert(&import.binds, &import.full);
            }
        }
    }
    let mut kept: BTreeMap<String, Import> = BTreeMap::new();
    for import in proposed {
        if conflicted.contains(&import.binds) {
            skipped.push(format!(
                "`{}` would name two different items in one file",
                import.binds
            ));
            continue;
        }
        kept.entry(import.full.clone()).or_insert(import);
    }
    kept
}

/// The import that shortens one path, or why the path keeps its qualification.
fn propose(scope: &Scope, path: &Path) -> Result<Import, String> {
    let names = path_names(path);
    let head = &names[0];
    let full_head = scope
        .resolve_head(head)
        .ok_or_else(|| format!("`{head}` is bound by no `use`, module, or crate root here"))?;

    let keep = keep_from(&names);
    if keep == 0 {
        return Err("nothing would be left to name the item by".to_owned());
    }
    if carries_arguments(path, keep) {
        return Err("a qualifying segment carries generic arguments".to_owned());
    }
    let binds = names[keep].clone();
    if scope.declared.contains(&binds) {
        return Err(format!("`{binds}` is already declared in this file"));
    }
    let full = join(&full_head, &names[1..=keep].join("::"));
    if let Some(binding) = scope.bindings.get(&binds)
        && binding.full != full
    {
        return Err(format!("`{binds}` already names `{}` here", binding.full));
    }
    if let Some((name, _)) = scope
        .bindings
        .iter()
        .find(|(name, binding)| binding.full == full && *name != &binds)
    {
        return Err(format!("this file already reads that item as `{name}`"));
    }
    Ok(Import { binds, full })
}

/// The index of the first segment that stays.
///
/// A type, trait, or constant reads as the item itself, so the import stops
/// there. An all-lowercase path names a function or a module instead, and
/// keeps its last module segment, which is what says where the call comes
/// from.
fn keep_from(names: &[String]) -> usize {
    names
        .iter()
        .enumerate()
        .skip(1)
        .find(|(_, name)| starts_upper(name))
        .map_or(names.len() - 2, |(idx, _)| idx)
}

/// One nesting of names: a file, or a module written inside one.
struct Level {
    /// Names bound here that a shortened path stopped travelling through, and
    /// what each shortening asked of them.
    orphans: BTreeMap<String, BTreeSet<String>>,
    /// Names bound here that something still reads.
    live: HashSet<String>,
    scope: Scope,
    /// Whether this level reads the names of the level around it.
    inherits: bool,
}

impl Level {
    fn new(scope: Scope, inherits: bool) -> Self {
        Self {
            scope,
            inherits,
            live: HashSet::new(),
            orphans: BTreeMap::new(),
        }
    }
}

/// The walk that shortens the file and counts what still reads each name.
///
/// Both answers come from one walk because they are the same question asked
/// twice: a `use` is worth keeping exactly when a path this pass did not
/// shorten still travels through it.
struct Pass<'a> {
    imports: &'a BTreeMap<String, Import>,
    /// Each import by the name it binds, which is what a path left standing
    /// has to be read against.
    by_name: HashMap<&'a str, &'a str>,
    /// Imports a rewritten file-level leaf now spells, which therefore need no
    /// line of their own.
    carried: HashSet<String>,
    /// Names an import cannot safely bind: the file spells the same item
    /// another way somewhere the import reaches, and a path this pass cannot
    /// recognise as that item is left reading one qualification too many.
    suspect: HashSet<String>,
    cuts: Vec<Range<usize>>,
    leaves: Vec<(Range<usize>, String)>,
}

impl Pass<'_> {
    /// Shorten one path, or record that it still reads the name it starts
    /// with.
    fn consider(
        &mut self,
        names: &[String],
        starts: &[usize],
        path: Option<&Path>,
        stack: &mut [Level],
        cutting: bool,
    ) {
        let Some((level, full_head)) = resolve(stack, &names[0]) else {
            return;
        };
        if cutting && let Some((cut, import)) = self.cut(names, starts, path, stack, &full_head) {
            self.cuts.push(cut);
            if let Some(suffix) = import.strip_prefix(&format!("{full_head}::")) {
                stack[level]
                    .orphans
                    .entry(names[0].clone())
                    .or_default()
                    .insert(suffix.to_owned());
            }
            return;
        }
        if names.len() >= 2 {
            stack[level].live.insert(names[0].clone());
            self.suspect(names, &full_head);
        }
    }

    /// The qualification one path loses once the imports are in, and the
    /// import that replaces it.
    fn cut(
        &self,
        names: &[String],
        starts: &[usize],
        path: Option<&Path>,
        stack: &[Level],
        full_head: &str,
    ) -> Option<(Range<usize>, String)> {
        let head_len = full_head.split("::").count();
        let mut full: Vec<&str> = full_head.split("::").collect();
        full.extend(names[1..].iter().map(String::as_str));

        for keep in (1..names.len()).rev() {
            let spelled = full[..head_len + keep].join("::");
            let Some(import) = self.imports.get(&spelled) else {
                continue;
            };
            if shadows(stack, &import.binds, &import.full) {
                return None;
            }
            if path.is_some_and(|path| carries_arguments(path, keep)) {
                return None;
            }
            if starts[0] >= starts[keep] {
                return None;
            }
            return Some((starts[0]..starts[keep], spelled));
        }
        None
    }

    /// Walk one level's items: its own paths and macro bodies, then the
    /// modules written inside it.
    fn items(&mut self, items: &[Item], stack: &mut Vec<Level>, cutting: bool) {
        let mut visitor = PathVisitor::default();
        for item in items {
            visitor.visit_item(item);
        }
        let here = stack.len() - 1;
        stack[here].live.extend(visitor.attr_names);
        for found in &visitor.found {
            let names = path_names(found.path);
            let starts = path_starts(found.path);
            self.consider(&names, &starts, Some(found.path), stack, cutting);
        }
        for mac in &visitor.macros {
            let mut chains = Vec::new();
            token_paths(mac.tokens.clone(), &mut chains);
            for chain in chains {
                self.consider(&chain.names, &chain.starts, None, stack, cutting);
            }
        }
        for module in visitor.modules {
            self.module(module, stack);
        }
    }

    /// Walk one inline module, which reads the level around it only when it
    /// opens itself to those names.
    ///
    /// Whatever the module reads out of the level around it keeps that name alive there, whether or
    /// not the module is walked further.
    fn module(&mut self, item: &ItemMod, stack: &mut Vec<Level>) {
        let Some((_, items)) = &item.content else {
            return;
        };
        let here = stack.len() - 1;
        let prefix = join(&stack[here].scope.prefix, &item.ident.to_string());
        let scope = Scope::of_items(items, &prefix);
        let inherits = scope.glob.inherited;
        let mut names = PathVisitor::default();
        for item in items {
            names.visit_item(item);
        }
        stack[here].live.extend(names.inherited_use);
        if !inherits {
            return;
        }
        let cutting = !scope.glob.foreign;
        stack.push(Level::new(scope, true));
        self.items(items, stack, cutting);
        let level = stack
            .pop()
            .unwrap_or_else(|| unreachable!("the level just pushed"));
        self.settle_level(&level, false);
    }

    /// Rewrite the leaves this level's shortenings left naming nothing.
    fn settle_level(&mut self, level: &Level, top: bool) {
        for (head, suffixes) in &level.orphans {
            if level.live.contains(head) {
                continue;
            }
            let Some(binding) = level.scope.bindings.get(head) else {
                continue;
            };
            let text = match &binding.leaf {
                Leaf::Name(range) => {
                    self.leaves
                        .push((range.clone(), render_name(head, suffixes)));
                    true
                }
                Leaf::Own(range) => {
                    self.leaves.push((range.clone(), render_own(suffixes)));
                    true
                }
                Leaf::None => false,
            };
            if text && top {
                for suffix in suffixes {
                    self.carried.insert(join(&binding.full, suffix));
                }
            }
        }
    }

    /// Note an import whose name this path spells out with a qualification
    /// this pass could not remove.
    ///
    /// Two spellings of one module - a crate's own re-export and the path it
    /// re-exports - read as different paths here and as one item to the
    /// compiler, which then reports the qualification the import made
    /// redundant. Nothing here can tell those apart from two modules that
    /// merely end in the same name, so the import goes rather than the path.
    fn suspect(&mut self, names: &[String], full_head: &str) {
        let head_len = full_head.split("::").count();
        let mut full: Vec<&str> = full_head.split("::").collect();
        full.extend(names[1..].iter().map(String::as_str));
        for (idx, name) in names.iter().enumerate().skip(1) {
            let Some(import) = self.by_name.get(name.as_str()) else {
                continue;
            };
            let at = head_len + idx - 1;
            let mine = &full[at - 1..=at];
            let theirs: Vec<&str> = import.rsplit("::").take(2).collect();
            if theirs.len() == 2 && mine[0] == theirs[1] && mine[1] == theirs[0] {
                self.suspect.insert(name.clone());
            }
        }
    }
}

/// Where the innermost level that can answer for `head` says it leads.
///
/// A name a `use` binds, or a module a level declares, beats the bare crate
/// root a level happens to have named elsewhere, however deep it was bound:
/// the root is only the reading left when nothing spells the name.
fn resolve(stack: &[Level], head: &str) -> Option<(usize, String)> {
    let mut root = None;
    for idx in (0..stack.len()).rev() {
        let level = &stack[idx];
        if let Some(binding) = level.scope.bindings.get(head) {
            return Some((idx, binding.full.clone()));
        }
        if level.scope.modules.contains(head) {
            return Some((idx, join(&level.scope.prefix, head)));
        }
        if root.is_none() && level.scope.roots.contains(head) {
            root = Some((idx, head.to_owned()));
        }
        if !level.inherits {
            break;
        }
    }
    root.or_else(|| is_crate_name(head).then(|| (0, head.to_owned())))
}

/// Whether a head nothing in the file binds is the crate it names.
///
/// An edition-2018 crate is in scope without a `use`, so most deep paths start
/// at a name no line of the file mentions. What else could stand there - a
/// generic parameter, an associated type, a type the file declares - is either
/// bound already or spelled in camel case, which the rule reads as a name and
/// leaves alone.
fn is_crate_name(head: &str) -> bool {
    let mut chars = head.strip_prefix("r#").unwrap_or(head).chars();
    chars.next().is_some_and(|first| first.is_ascii_lowercase())
        && chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
}

/// Whether a name an import would bind already means something else where the
/// path stands.
fn shadows(stack: &[Level], binds: &str, full: &str) -> bool {
    for level in stack.iter().rev() {
        if level.scope.declared.contains(binds) {
            return true;
        }
        if let Some(binding) = level.scope.bindings.get(binds) {
            return binding.full != full;
        }
        if !level.inherits {
            return false;
        }
    }
    false
}

/// The `use` leaf `head` becomes once it carries the imports it used to
/// qualify.
fn render_name(head: &str, suffixes: &BTreeSet<String>) -> String {
    format!("{head}::{}", render_own(suffixes))
}

/// The same, for a leaf already written inside the group its name prefixes.
fn render_own(suffixes: &BTreeSet<String>) -> String {
    let mut names = suffixes.iter();
    match (names.next(), names.len()) {
        (Some(only), 0) => only.clone(),
        _ => format!(
            "{{{}}}",
            suffixes.iter().cloned().collect::<Vec<_>>().join(", ")
        ),
    }
}

/// What a scope already says about the names a path can start with.
#[derive(Clone)]
struct Scope {
    glob: Glob,
    /// Name a `use` binds, to what it binds it to.
    bindings: HashMap<String, Binding>,
    /// Names the scope declares itself, which an import must not shadow.
    declared: HashSet<String>,
    /// Modules the scope declares, reachable through its own prefix.
    modules: HashSet<String>,
    /// Roots the scope's own `use` declarations start from: a crate it can
    /// already name, so a path may start there too.
    roots: HashSet<String>,
    /// How this scope spells itself, so that two levels naming one module do
    /// not read as the same path.
    prefix: String,
}

/// Where a scope's glob imports come from.
///
/// A glob from `self` or `super` opens the scope to the names around it; one
/// from anywhere else brings in names nothing here can enumerate.
#[derive(Default, Clone, Copy)]
struct Glob {
    foreign: bool,
    inherited: bool,
}

/// One name a `use` brings in.
#[derive(Clone)]
struct Binding {
    leaf: Leaf,
    full: String,
}

/// Where a bound name is spelled, and how an import takes its place.
#[derive(Clone)]
enum Leaf {
    /// `use a::b;` - the ident names the item, and an import extends it.
    Name(Range<usize>),
    /// `use a::b::{self, C};` - `self` names `a::b` from inside a group that
    /// already spells it, so an import stands where the `self` did.
    Own(Range<usize>),
    /// An exported, conditional, or renamed `use` answers to more than the
    /// paths in this file, so it is left as written.
    None,
}

impl Scope {
    /// `self` inside a group names the module the group is written under.
    fn absorb_own(&mut self, prefix: &str, range: Range<usize>, rewritable: bool) {
        let Some(name) = prefix.rsplit("::").next() else {
            return;
        };
        if name.is_empty() {
            return;
        }
        let leaf = if rewritable {
            Leaf::Own(range)
        } else {
            Leaf::None
        };
        self.bindings.insert(
            name.to_owned(),
            Binding {
                leaf,
                full: prefix.to_owned(),
            },
        );
    }

    fn absorb_use(&mut self, prefix: &str, tree: &UseTree, rewritable: bool) {
        match tree {
            UseTree::Path(value) => {
                let name = value.ident.to_string();
                if prefix.is_empty() {
                    self.roots.insert(name.clone());
                }
                let next = join(prefix, &name);
                self.absorb_use(&next, &value.tree, rewritable);
            }
            UseTree::Name(value) => {
                let name = value.ident.to_string();
                let range = value.ident.span().byte_range();
                if name == "self" {
                    self.absorb_own(prefix, range, rewritable);
                    return;
                }
                if prefix.is_empty() {
                    self.roots.insert(name.clone());
                }
                let leaf = if rewritable {
                    Leaf::Name(range)
                } else {
                    Leaf::None
                };
                self.bindings.insert(
                    name.clone(),
                    Binding {
                        leaf,
                        full: join(prefix, &name),
                    },
                );
            }
            UseTree::Rename(value) => {
                let name = value.ident.to_string();
                if prefix.is_empty() {
                    self.roots.insert(name.clone());
                }
                let full = if name == "self" {
                    prefix.to_owned()
                } else {
                    join(prefix, &name)
                };
                self.bindings.insert(
                    value.rename.to_string(),
                    Binding {
                        full,
                        leaf: Leaf::None,
                    },
                );
            }
            UseTree::Glob(_) => {
                let root = prefix.split("::").next().unwrap_or_default();
                if matches!(root, "self" | "super") {
                    self.glob.inherited = true;
                } else {
                    self.glob.foreign = true;
                }
            }
            UseTree::Group(value) => {
                for item in &value.items {
                    self.absorb_use(prefix, item, rewritable);
                }
            }
        }
    }

    fn of_items(items: &[Item], prefix: &str) -> Self {
        let mut scope = Self {
            bindings: HashMap::new(),
            roots: HashSet::new(),
            modules: HashSet::new(),
            declared: HashSet::new(),
            prefix: prefix.to_owned(),
            glob: Glob::default(),
        };
        for item in items {
            match item {
                Item::Use(value) => {
                    let rewritable = value.attrs.is_empty()
                        && matches!(value.vis, syn::Visibility::Inherited)
                        && value.leading_colon.is_none();
                    scope.absorb_use("", &value.tree, rewritable);
                }
                Item::Mod(value) => {
                    scope.modules.insert(value.ident.to_string());
                    scope.declared.insert(value.ident.to_string());
                }
                other => {
                    if let Some(name) = declared_name(other) {
                        scope.declared.insert(name);
                    }
                }
            }
        }
        scope
    }

    /// The full path the scope's first segment stands for.
    fn resolve_head(&self, head: &str) -> Option<String> {
        if let Some(binding) = self.bindings.get(head) {
            return Some(binding.full.clone());
        }
        if self.modules.contains(head) {
            return Some(join(&self.prefix, head));
        }
        if self.roots.contains(head) || is_crate_name(head) {
            return Some(head.to_owned());
        }
        None
    }
}

fn declared_name(item: &Item) -> Option<String> {
    match item {
        Item::Const(value) => Some(value.ident.to_string()),
        Item::Enum(value) => Some(value.ident.to_string()),
        Item::Fn(value) => Some(value.sig.ident.to_string()),
        Item::Static(value) => Some(value.ident.to_string()),
        Item::Struct(value) => Some(value.ident.to_string()),
        Item::Trait(value) => Some(value.ident.to_string()),
        Item::TraitAlias(value) => Some(value.ident.to_string()),
        Item::Type(value) => Some(value.ident.to_string()),
        Item::Union(value) => Some(value.ident.to_string()),
        _ => None,
    }
}

fn join(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_owned()
    } else {
        format!("{prefix}::{name}")
    }
}

/// A `name::name::name` chain a macro body spells out.
///
/// A macro is opaque tokens to `syn`, but the compiler expands it here and
/// reads the paths inside it, so a qualification left standing in one is
/// reported like any other.
struct TokenPath {
    names: Vec<String>,
    starts: Vec<usize>,
}

fn token_paths(tokens: TokenStream, out: &mut Vec<TokenPath>) {
    let trees: Vec<TokenTree> = tokens.into_iter().collect();
    let mut idx = 0;
    let mut after_dot = false;
    while idx < trees.len() {
        match &trees[idx] {
            TokenTree::Group(group) => {
                token_paths(group.stream(), out);
                after_dot = false;
                idx += 1;
            }
            TokenTree::Punct(punct) => {
                after_dot = matches!(punct.as_char(), '.' | '$');
                idx += 1;
            }
            TokenTree::Ident(ident) if !after_dot => {
                let mut chain = TokenPath {
                    names: vec![ident.to_string()],
                    starts: vec![ident.span().byte_range().start],
                };
                idx = extend_chain(&trees, idx + 1, &mut chain);
                if chain.names.len() >= 2 && !breaks_chain(trees.get(idx)) {
                    out.push(chain);
                }
            }
            TokenTree::Ident(_) | TokenTree::Literal(_) => {
                after_dot = false;
                idx += 1;
            }
        }
    }
}

/// Follow `:: name` for as long as the tokens spell one, and answer where the
/// chain stops.
fn extend_chain(trees: &[TokenTree], mut idx: usize, chain: &mut TokenPath) -> usize {
    while idx + 2 < trees.len() {
        let joined = matches!(&trees[idx], TokenTree::Punct(p)
            if p.as_char() == ':' && p.spacing() == Spacing::Joint);
        let colon = matches!(&trees[idx + 1], TokenTree::Punct(p) if p.as_char() == ':');
        let TokenTree::Ident(next) = &trees[idx + 2] else {
            break;
        };
        if !joined || !colon {
            break;
        }
        chain.names.push(next.to_string());
        chain.starts.push(next.span().byte_range().start);
        idx += 3;
    }
    idx
}

/// Whether what follows says the chain was never a path: a macro call, or a
/// turbofish whose `::` leads to no name.
fn breaks_chain(next: Option<&TokenTree>) -> bool {
    matches!(next, Some(TokenTree::Punct(p)) if matches!(p.as_char(), '!' | ':'))
}

fn carries_arguments(path: &Path, keep: usize) -> bool {
    path.segments
        .iter()
        .take(keep)
        .any(|segment| !segment.arguments.is_none())
}

fn path_names(path: &Path) -> Vec<String> {
    path.segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect()
}

fn path_starts(path: &Path) -> Vec<usize> {
    path.segments
        .iter()
        .map(|segment| segment.ident.span().byte_range().start)
        .collect()
}

fn starts_upper(name: &str) -> bool {
    name.chars().next().is_some_and(char::is_uppercase)
}

fn print_path(path: &Path) -> String {
    path_names(path).join("::")
}

/// The end of the file's last top-level `use`, where a new one can go. The
/// formatter sorts and groups it from there.
fn last_use_end(file: &syn::File) -> Option<usize> {
    file.items
        .iter()
        .filter_map(|item| match item {
            Item::Use(value) => Some(value.span().byte_range().end),
            _ => None,
        })
        .max()
}

/// Where the first item starts, which is where a file with no `use` takes one.
///
/// An item spans its own attributes, so the import lands above the doc comment
/// that introduces the item rather than between the two.
fn first_item_start(file: &syn::File) -> Option<usize> {
    file.items
        .first()
        .map(|item| item.span().byte_range().start)
}

/// One path a scope spells out, and whether every configuration compiles it.
struct Found<'ast> {
    path: &'ast Path,
    gated: bool,
}

/// Everything one scope spells, stopping at the modules written inside it:
/// those read their own names and are walked with their own scope.
#[derive(Default)]
struct PathVisitor<'ast> {
    /// Names an attribute spells. Nothing here can tell which of an
    /// attribute's tokens is a path, so all of them keep their names alive.
    attr_names: HashSet<String>,
    /// Names this scope reads out of the one around it, which keep those
    /// names alive wherever they are bound.
    inherited_use: HashSet<String>,
    found: Vec<Found<'ast>>,
    macros: Vec<&'ast Macro>,
    modules: Vec<&'ast ItemMod>,
    /// A `use` inside a body binds names this scope cannot see, so no path in
    /// it resolves reliably.
    local_use: bool,
    /// How many enclosing items are gated. A `use` at the top of the file is
    /// not, so a path only some configurations compile does not earn an
    /// import the other configurations would call unused.
    gated: usize,
}

impl PathVisitor<'_> {
    fn absorb_inherited(&mut self, prefix: &str, tree: &UseTree) {
        match tree {
            UseTree::Path(value) => {
                let name = value.ident.to_string();
                if matches!(prefix, "self" | "super") {
                    self.inherited_use.insert(name.clone());
                }
                let next = if prefix.is_empty() {
                    name
                } else {
                    prefix.to_owned()
                };
                self.absorb_inherited(&next, &value.tree);
            }
            UseTree::Name(value) => {
                if matches!(prefix, "self" | "super") {
                    self.inherited_use.insert(value.ident.to_string());
                }
            }
            UseTree::Rename(value) => {
                if matches!(prefix, "self" | "super") {
                    self.inherited_use.insert(value.ident.to_string());
                }
            }
            UseTree::Glob(_) => {}
            UseTree::Group(value) => {
                for item in &value.items {
                    self.absorb_inherited(prefix, item);
                }
            }
        }
    }

    fn gate(&mut self, gated: bool, walk: impl FnOnce(&mut Self)) {
        self.gated += usize::from(gated);
        walk(self);
        self.gated -= usize::from(gated);
    }

    fn under_cfg(&mut self, attrs: &[syn::Attribute], walk: impl FnOnce(&mut Self)) {
        self.gate(gated_by(attrs), walk);
    }
}

impl<'ast> Visit<'ast> for PathVisitor<'ast> {
    fn visit_arm(&mut self, arm: &'ast Arm) {
        self.under_cfg(&arm.attrs, |this| visit::visit_arm(this, arm));
    }

    fn visit_attribute(&mut self, attr: &'ast syn::Attribute) {
        if attr.path().is_ident("doc") {
            return;
        }
        absorb_idents(attr.to_token_stream(), &mut self.attr_names);
    }

    fn visit_field(&mut self, field: &'ast Field) {
        self.under_cfg(&field.attrs, |this| visit::visit_field(this, field));
    }

    fn visit_impl_item(&mut self, item: &'ast ImplItem) {
        let attrs = impl_item_attrs(item);
        self.under_cfg(attrs, |this| visit::visit_impl_item(this, item));
    }

    fn visit_item(&mut self, item: &'ast Item) {
        let attrs = item_attrs(item);
        self.under_cfg(attrs, |this| visit::visit_item(this, item));
    }

    /// A `macro_rules!` body is expanded wherever the macro is called, so this file's own imports
    /// do not travel there and cannot shorten paths inside it.
    fn visit_item_macro(&mut self, item: &'ast syn::ItemMacro) {
        if item.ident.is_none() {
            visit::visit_item_macro(self, item);
        }
    }

    fn visit_item_mod(&mut self, item: &'ast ItemMod) {
        if item.content.is_some() {
            self.modules.push(item);
            return;
        }
        visit::visit_item_mod(self, item);
    }

    fn visit_item_use(&mut self, item: &'ast ItemUse) {
        self.absorb_inherited("", &item.tree);
    }

    fn visit_macro(&mut self, mac: &'ast Macro) {
        self.macros.push(mac);
        visit::visit_macro(self, mac);
    }

    fn visit_path(&mut self, path: &'ast Path) {
        if path.leading_colon.is_none() {
            self.found.push(Found {
                path,
                gated: self.gated > 0,
            });
        }
        visit::visit_path(self, path);
    }

    fn visit_stmt(&mut self, stmt: &'ast Stmt) {
        let gated = match stmt {
            Stmt::Item(Item::Use(_)) => {
                self.local_use = true;
                false
            }
            Stmt::Item(_) => false,
            Stmt::Local(value) => gated_by(&value.attrs),
            Stmt::Macro(value) => gated_by(&value.attrs),
            Stmt::Expr(value, _) => gated_expr(value),
        };
        self.gate(gated, |this| visit::visit_stmt(this, stmt));
    }

    fn visit_trait_item(&mut self, item: &'ast TraitItem) {
        let attrs = trait_item_attrs(item);
        self.under_cfg(attrs, |this| visit::visit_trait_item(this, item));
    }

    fn visit_variant(&mut self, variant: &'ast Variant) {
        self.under_cfg(&variant.attrs, |this| visit::visit_variant(this, variant));
    }
}

/// Whether these attributes hold a path only some configurations compile.
fn gated_by(attrs: &[syn::Attribute]) -> bool {
    attrs
        .iter()
        .any(|attr| attr.path().is_ident("cfg") || attr.path().is_ident("cfg_attr"))
}

/// Whether an expression standing as a statement carries a `#[cfg]`.
///
/// A body gates a block or a call the same way an item is gated, and a path
/// inside one compiles only in some configurations. `syn` keeps an
/// expression's attributes inside the expression, which is non-exhaustive and
/// hands out no accessor, so the answer comes off the front of its own tokens,
/// where an outer attribute always stands.
fn gated_expr(expr: &Expr) -> bool {
    let mut tokens = expr.to_token_stream().into_iter().peekable();
    while matches!(tokens.peek(), Some(TokenTree::Punct(punct)) if punct.as_char() == '#') {
        tokens.next();
        let Some(TokenTree::Group(group)) = tokens.next() else {
            return false;
        };
        if matches!(
            group.stream().into_iter().next(),
            Some(TokenTree::Ident(name)) if name == "cfg" || name == "cfg_attr"
        ) {
            return true;
        }
    }
    false
}

fn absorb_idents(tokens: TokenStream, names: &mut HashSet<String>) {
    for tree in tokens {
        match tree {
            TokenTree::Ident(ident) => {
                names.insert(ident.to_string());
            }
            TokenTree::Group(group) => absorb_idents(group.stream(), names),
            TokenTree::Literal(literal) => {
                for word in literal
                    .to_string()
                    .split(|ch: char| !ch.is_alphanumeric() && ch != '_')
                {
                    if !word.is_empty() {
                        names.insert(word.to_owned());
                    }
                }
            }
            TokenTree::Punct(_) => {}
        }
    }
}

fn impl_item_attrs(item: &ImplItem) -> &[syn::Attribute] {
    match item {
        ImplItem::Const(value) => &value.attrs,
        ImplItem::Fn(value) => &value.attrs,
        ImplItem::Type(value) => &value.attrs,
        ImplItem::Macro(value) => &value.attrs,
        _ => &[],
    }
}

fn item_attrs(item: &Item) -> &[syn::Attribute] {
    match item {
        Item::Const(value) => &value.attrs,
        Item::Enum(value) => &value.attrs,
        Item::ExternCrate(value) => &value.attrs,
        Item::Fn(value) => &value.attrs,
        Item::ForeignMod(value) => &value.attrs,
        Item::Impl(value) => &value.attrs,
        Item::Macro(value) => &value.attrs,
        Item::Mod(value) => &value.attrs,
        Item::Static(value) => &value.attrs,
        Item::Struct(value) => &value.attrs,
        Item::Trait(value) => &value.attrs,
        Item::TraitAlias(value) => &value.attrs,
        Item::Type(value) => &value.attrs,
        Item::Union(value) => &value.attrs,
        Item::Use(value) => &value.attrs,
        _ => &[],
    }
}

fn trait_item_attrs(item: &TraitItem) -> &[syn::Attribute] {
    match item {
        TraitItem::Const(value) => &value.attrs,
        TraitItem::Fn(value) => &value.attrs,
        TraitItem::Type(value) => &value.attrs,
        TraitItem::Macro(value) => &value.attrs,
        _ => &[],
    }
}

fn is_deep(cfg: &QualifiedPathDepthConfig, path: &Path) -> bool {
    if path.leading_colon.is_some() || path.segments.len() <= cfg.max_segments {
        return false;
    }
    let head = path.segments[0].ident.to_string();
    !cfg.exempt_roots.contains(&head) && !starts_upper(&head)
}

impl QualifiedPathDepthConfig {
    fn covers(&self, rel: &str) -> bool {
        let included = compile_globs(&self.include_paths);
        let excluded = compile_globs(&self.exclude_paths);
        let rel = FsPath::new(rel);
        matches_any(&included, rel) && !matches_any(&excluded, rel)
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    /// The snippet as the fix leaves it, and the qualifications it kept.
    fn fix(src: &str) -> (String, Vec<String>) {
        let cfg = QualifiedPathDepthConfig::default();
        let rewrite = rewrite(&cfg, src).expect("the fix rewrites or refuses, never fails");
        (
            rewrite.source.unwrap_or_else(|| src.to_owned()),
            rewrite.skipped,
        )
    }

    fn deep(src: &str) -> Vec<String> {
        let cfg = QualifiedPathDepthConfig::default();
        let file = syn::parse_file(src).expect("parse");
        let mut visitor = PathVisitor::default();
        visitor.visit_file(&file);
        visitor
            .found
            .iter()
            .filter(|found| !found.gated && is_deep(&cfg, found.path))
            .map(|found| print_path(found.path))
            .collect()
    }

    #[test]
    fn a_path_through_a_bound_module_becomes_the_item_it_names() {
        let src = "use iced::mouse;\nfn f() -> u8 {\n    mouse::Event::ButtonPressed\n}\n";

        let (out, skipped) = fix(src);

        assert!(skipped.is_empty(), "skipped: {skipped:?}");
        assert!(out.contains("use iced::mouse::Event;"), "{out}");
        assert!(out.contains("Event::ButtonPressed"), "{out}");
        assert!(!out.contains("mouse::Event::ButtonPressed"), "{out}");
    }

    #[test]
    fn the_use_a_shortened_path_stops_travelling_through_carries_the_import() {
        let src = "use iced::mouse;\nfn f() -> u8 {\n    mouse::Event::ButtonPressed\n}\n";

        let (out, _) = fix(src);

        assert_eq!(
            out.matches("use ").count(),
            1,
            "the orphaned `use` becomes the import rather than joining it: {out}"
        );
        assert!(!out.contains("use iced::mouse;"), "{out}");
    }

    #[test]
    fn a_use_still_read_elsewhere_keeps_its_name_and_gains_a_neighbour() {
        let src = "use iced::mouse;\nfn f(_: mouse::Cursor) -> u8 {\n    mouse::Event::ButtonPressed\n}\n";

        let (out, _) = fix(src);

        assert!(out.contains("use iced::mouse;"), "{out}");
        assert!(out.contains("use iced::mouse::Event;"), "{out}");
        assert!(out.contains("_: mouse::Cursor"), "{out}");
    }

    #[test]
    fn one_use_carries_every_import_it_was_the_road_to() {
        let src = "use iced::mouse;\nfn f() -> u8 {\n    mouse::Event::ButtonPressed;\n    mouse::click::Kind::Single\n}\n";

        let (out, skipped) = fix(src);

        assert!(skipped.is_empty(), "skipped: {skipped:?}");
        assert!(
            out.contains("use iced::mouse::{Event, click::Kind};"),
            "{out}"
        );
    }

    #[test]
    fn a_shallow_path_to_an_imported_item_reads_short_too() {
        let src =
            "use iced::widget::canvas;\nfn f() -> canvas::Cache {\n    canvas::Cache::new()\n}\n";

        let (out, skipped) = fix(src);

        assert!(skipped.is_empty(), "skipped: {skipped:?}");
        assert!(out.contains("use iced::widget::canvas::Cache;"), "{out}");
        assert!(
            !out.contains("-> canvas::Cache"),
            "an import that leaves `canvas::Cache` standing is an unnecessary qualification: {out}"
        );
        assert!(out.contains("-> Cache"), "{out}");
    }

    #[test]
    fn a_path_through_a_crate_the_file_already_names_resolves_to_that_crate() {
        let src = "use iced::widget::Text;\nfn f() -> Text {\n    iced::mouse::Cursor::new()\n}\n";

        let (out, skipped) = fix(src);

        assert!(skipped.is_empty(), "skipped: {skipped:?}");
        assert!(out.contains("use iced::mouse::Cursor;"), "{out}");
        assert!(out.contains("Cursor::new()"), "{out}");
    }

    #[test]
    fn a_path_through_a_module_the_file_declares_reads_through_self() {
        let src = "use crate::X;\nmod solve;\nfn f() -> X {\n    solve::Length::Fill\n}\n";

        let (out, skipped) = fix(src);

        assert!(skipped.is_empty(), "skipped: {skipped:?}");
        assert!(out.contains("use self::solve::Length;"), "{out}");
        assert!(out.contains("Length::Fill"), "{out}");
    }

    #[test]
    fn an_all_lowercase_path_keeps_the_module_that_says_where_it_comes_from() {
        let src = "use iced::mouse;\nfn f() {\n    mouse::cursor::position();\n}\n";

        let (out, skipped) = fix(src);

        assert!(skipped.is_empty(), "skipped: {skipped:?}");
        assert!(out.contains("use iced::mouse::cursor;"), "{out}");
        assert!(out.contains("cursor::position()"), "{out}");
    }

    #[test]
    fn a_name_the_file_already_spells_for_something_else_keeps_its_path() {
        let src = "use iced::mouse;\nuse other::Event;\nfn f() -> Event {\n    mouse::Event::ButtonPressed\n}\n";

        let (out, skipped) = fix(src);

        assert_eq!(out, src, "the shortened path would name the wrong Event");
        assert!(
            skipped.iter().any(|s| s.contains("already names")),
            "{skipped:?}"
        );
    }

    #[test]
    fn two_paths_that_would_bind_one_name_to_two_items_both_stay() {
        let src = "use a::one;\nuse a::two;\nfn f() {\n    one::Event::X;\n    two::Event::Y;\n}\n";

        let (out, skipped) = fix(src);

        assert_eq!(out, src);
        assert!(
            skipped.iter().any(|s| s.contains("two different items")),
            "{skipped:?}"
        );
    }

    #[test]
    fn a_head_no_line_of_the_file_names_is_the_crate_it_names() {
        let src = "use std::fmt;\nfn f() {\n    unknown::Thing::make();\n}\n";

        let (out, skipped) = fix(src);

        assert!(skipped.is_empty(), "skipped: {skipped:?}");
        assert!(out.contains("use unknown::Thing;"), "{out}");
        assert!(out.contains("Thing::make()"), "{out}");
        assert!(!out.contains("unknown::Thing::make"), "{out}");
    }

    #[test]
    fn a_file_with_no_use_takes_its_first_one_above_the_item() {
        let src = "/// Doc.\nfn f() {\n    unknown::Thing::make();\n}\n";

        let (out, skipped) = fix(src);

        assert!(skipped.is_empty(), "skipped: {skipped:?}");
        assert_eq!(
            out, "use unknown::Thing;\n\n/// Doc.\nfn f() {\n    Thing::make();\n}\n",
            "{out}"
        );
    }

    #[test]
    fn a_head_that_no_crate_could_be_called_keeps_its_path() {
        let src = "use std::fmt;\nfn f() {\n    _hidden::Thing::make();\n}\n";

        let (out, skipped) = fix(src);

        assert_eq!(out, src);
        assert!(
            skipped.iter().any(|s| s.contains("bound by no `use`")),
            "{skipped:?}"
        );
    }

    #[test]
    fn a_glob_import_stops_the_file_from_being_resolved() {
        let src = "use iced::mouse;\nuse prelude::*;\nfn f() {\n    mouse::Event::X;\n}\n";

        let (out, skipped) = fix(src);

        assert_eq!(out, src);
        assert!(skipped.iter().any(|s| s.contains("glob")), "{skipped:?}");
    }

    #[test]
    fn a_use_inside_a_body_leaves_the_file_alone() {
        let src = "use iced::mouse;\nfn f() {\n    use other::mouse;\n    mouse::Event::X;\n}\n";

        let (out, skipped) = fix(src);

        assert_eq!(out, src);
        assert!(
            skipped.iter().any(|s| s.contains("inside a body")),
            "{skipped:?}"
        );
    }

    #[test]
    fn an_exported_use_is_not_rewritten_into_an_import() {
        let src = "pub use iced::mouse;\nfn f() {\n    mouse::Event::X;\n}\n";

        let (out, _) = fix(src);

        assert!(out.contains("pub use iced::mouse;"), "{out}");
        assert!(out.contains("use iced::mouse::Event;"), "{out}");
    }

    #[test]
    fn a_name_an_inline_module_still_reads_keeps_its_use() {
        let src = "use iced::mouse;\nfn f() {\n    mouse::Event::X;\n}\n#[cfg(test)]\nmod tests {\n    use super::mouse;\n    fn g() -> mouse::Cursor { todo!() }\n}\n";

        let (out, _) = fix(src);

        assert!(
            out.contains("use iced::mouse;"),
            "the module below still travels through it: {out}"
        );
        assert!(out.contains("use iced::mouse::Event;"), "{out}");
    }

    #[test]
    fn a_name_only_a_macro_reads_keeps_its_use() {
        let src = "use iced::mouse;\nfn f() {\n    mouse::Event::X;\n    println!(\"{:?}\", mouse::DEFAULT);\n}\n";

        let (out, _) = fix(src);

        assert!(out.contains("use iced::mouse;"), "{out}");
        assert!(out.contains("use iced::mouse::Event;"), "{out}");
    }

    #[test]
    fn a_declaration_the_file_owns_is_not_shadowed() {
        let src = "use iced::mouse;\nstruct Event;\nfn f() -> Event {\n    mouse::Event::X;\n    Event\n}\n";

        let (out, skipped) = fix(src);

        assert_eq!(out, src);
        assert!(
            skipped.iter().any(|s| s.contains("already declared")),
            "{skipped:?}"
        );
    }

    #[test]
    fn the_roots_a_path_carries_its_own_meaning_from_are_left_alone() {
        let src = "fn f() {\n    self::a::B;\n    super::a::B;\n    crate::a::B;\n    std::sync::Arc::new(1);\n    tokio::sync::mpsc::channel(1);\n}\n";

        assert!(deep(src).is_empty(), "{:?}", deep(src));
    }

    #[test]
    fn a_path_only_some_configurations_compile_keeps_its_qualification() {
        let src = "use iced::mouse;\n#[cfg(feature = \"x\")]\nfn f() {\n    mouse::Event::X;\n}\n";

        assert!(deep(src).is_empty(), "{:?}", deep(src));
    }

    #[test]
    fn a_two_segment_path_is_already_short() {
        assert!(deep("fn f() { mouse::Event; }").is_empty());
    }

    #[test]
    fn an_attribute_is_not_a_path_the_reader_follows() {
        assert!(deep("#[serde(with = \"a::b::c\")]\nstruct X;").is_empty());
    }

    #[test]
    fn an_inline_module_carries_its_own_imports() {
        let src = "use iced::mouse;\nmod inner {\n    pub fn f() { other::Event::X; }\n}\n";

        assert!(deep(src).is_empty(), "{:?}", deep(src));
    }

    #[test]
    fn shortening_a_path_leaves_nothing_left_to_shorten() {
        let src = "use iced::mouse;\nfn f() {\n    mouse::Event::ButtonPressed;\n}\n";

        let (once, _) = fix(src);
        let (twice, _) = fix(&once);

        assert_eq!(once, twice, "the fix reaches a fixed point");
    }
    #[test]
    fn a_gated_match_arm_does_not_earn_an_import() {
        let src = "use kithara_decode::Backend as B;\nfn f(b: B) -> u8 {\n    match b {\n        #[cfg(feature = \"apple\")]\n        kithara_decode::Backend::Apple => 1,\n        _ => 0,\n    }\n}\n";

        let (out, _) = fix(src);

        assert_eq!(out, src, "an import the other builds call unused: {out}");
    }

    #[test]
    fn a_gated_block_in_a_body_does_not_earn_an_import() {
        let src = "fn f() {\n    #[cfg(feature = \"render\")]\n    {\n        naga::valid::Validator::new();\n    }\n}\n";

        let (out, _) = fix(src);

        assert_eq!(out, src, "an import the other builds call unused: {out}");
    }

    #[test]
    fn a_gated_call_in_a_body_does_not_earn_an_import() {
        let src = "fn f() {\n    #[cfg(unix)]\n    naga::valid::Validator::new();\n}\n";

        let (out, _) = fix(src);

        assert_eq!(out, src, "an import the other builds call unused: {out}");
    }

    #[test]
    fn an_ungated_call_in_a_body_earns_the_import_a_gated_one_refuses() {
        let src = "fn f() {\n    naga::valid::Validator::new();\n}\n";

        let (out, _) = fix(src);

        assert!(out.contains("use naga::valid::Validator;"), "{out}");
    }

    #[test]
    fn an_item_the_file_already_renames_keeps_its_path() {
        let src = "use kithara_decode::Backend as B;\nfn f() -> B {\n    kithara_decode::Backend::Apple\n}\n";

        let (out, skipped) = fix(src);

        assert_eq!(out, src);
        assert!(
            skipped
                .iter()
                .any(|s| s.contains("already reads that item")),
            "{skipped:?}"
        );
    }

    #[test]
    fn a_module_that_spells_the_same_item_another_way_cancels_the_import() {
        let src = "use iced::advanced::mouse;\nfn f() -> u8 {\n    mouse::Interaction::None\n}\n#[cfg(test)]\nmod tests {\n    use iced::mouse;\n\n    use super::*;\n\n    fn g() -> u8 { mouse::Interaction::Pointer }\n}\n";

        let (out, skipped) = fix(src);

        assert_eq!(
            out, src,
            "an import the module's own spelling would over-qualify: {out}"
        );
        assert!(
            skipped.iter().any(|s| s.contains("another way")),
            "{skipped:?}"
        );
    }

    #[test]
    fn a_module_that_reads_the_file_shortens_with_it() {
        let src = "use iced::advanced::mouse;\nfn f() -> u8 {\n    mouse::Interaction::None\n}\n#[cfg(test)]\nmod tests {\n    use super::*;\n\n    fn g() -> u8 { mouse::Interaction::Pointer }\n}\n";

        let (out, skipped) = fix(src);

        assert!(skipped.is_empty(), "skipped: {skipped:?}");
        assert!(out.contains("Interaction::Pointer"), "{out}");
        assert!(
            !out.contains("{ mouse::Interaction"),
            "the module reads what the file imported: {out}"
        );
    }

    #[test]
    fn a_qualification_inside_a_macro_body_is_shortened_too() {
        let src = "use iced::window;\nfn f() {\n    let _ = window::RedrawRequest::Wait;\n    assert_eq!(one(), window::RedrawRequest::Wait);\n}\nfn one() -> u8 { 0 }\n";

        let (out, skipped) = fix(src);

        assert!(skipped.is_empty(), "skipped: {skipped:?}");
        assert_eq!(
            out.matches("window::RedrawRequest::Wait").count(),
            0,
            "the compiler reads a macro body like any other code: {out}"
        );
        assert_eq!(out.matches("RedrawRequest::Wait").count(), 2, "{out}");
    }

    #[test]
    fn a_self_leaf_carries_the_import_from_inside_its_own_group() {
        let src = "use iced::advanced::mouse::{self, Cursor};\nfn f(_: Cursor) -> u8 {\n    mouse::Interaction::None\n}\n";

        let (out, skipped) = fix(src);

        assert!(skipped.is_empty(), "skipped: {skipped:?}");
        assert!(
            out.contains("use iced::advanced::mouse::{Interaction, Cursor};"),
            "{out}"
        );
        assert!(out.contains("Interaction::None"), "{out}");
    }
}

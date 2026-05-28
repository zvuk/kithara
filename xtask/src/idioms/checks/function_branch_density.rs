use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

use anyhow::Result;
use syn::{
    BinOp, Expr, ExprBinary, ExprCall, ExprIf, ExprMatch, ExprMethodCall, ExprTry, ImplItem, Item,
    ItemImpl, Local, Stmt, Type, Visibility,
    spanned::Spanned,
    visit::{self, Visit},
};

use super::{Check, Context};
use crate::common::{
    parse::parse_file,
    violation::Violation,
    walker::{compile_globs, matches_any, relative_to, workspace_rs_files_scoped},
};

pub(crate) const ID: &str = "function_branch_density";

/// Checked-in call graph emitted by `cargo xtask callgraph`. When present, the
/// walk follows these real direct-call edges; absent or unparsable, the static
/// type-aware resolver (`walk_density`) is used instead.
const CALLGRAPH_REL: &str = ".config/idioms/callgraph.json";

pub(crate) struct FunctionBranchDensity;

/// A call-graph node key, the `"rel_file:line"` label used by `callgraph.json`.
type NodeKey = String;

/// Resolution key for a call site or definition. Resolution is purely
/// syntactic: a free function resolves by name, a method resolves by the
/// `(receiver type, method name)` pair. Anything whose receiver type is
/// not syntactically known is dropped at the call site (UNRESOLVED) rather
/// than fanned out across every same-named function in the workspace.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum FnKey {
    Free(String),
    Method { ty: String, name: String },
}

#[derive(Debug, Clone)]
struct FnInfo {
    /// Branches in *this* function's body (no descent).
    own_branches: usize,
    /// Resolved keys of called functions. Unresolved calls are dropped
    /// (not traversed) — never inflated via name collision.
    calls: Vec<FnKey>,
    /// `(rel_path, line, col)` for the function declaration.
    span: (String, usize, usize),
    /// `pub` / `pub(crate)` / `pub(super)` etc. — only public entries
    /// drive the walk.
    is_public: bool,
    /// Registry key for this definition. Used by the cycle guard.
    key: FnKey,
}

impl Check for FunctionBranchDensity {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.function_branch_density;
        let exempt = compile_globs(&cfg.exempt_files);
        let mut registry: HashMap<FnKey, Vec<FnInfo>> = HashMap::new();
        for path in workspace_rs_files_scoped(ctx.workspace_root, ctx.scope)? {
            let rel_path = relative_to(ctx.workspace_root, &path).to_path_buf();
            let rel = rel_path.to_string_lossy().replace('\\', "/");
            if matches_any(&exempt, Path::new(&rel)) {
                continue;
            }
            let Ok(file) = parse_file(&path) else {
                continue;
            };
            collect_functions(&file, &rel, &mut registry);
        }

        let graph = load_callgraph(&ctx.workspace_root.join(CALLGRAPH_REL));
        let branches = graph.as_ref().map(|g| g.branches_by_node(&registry));
        let mut violations = Vec::new();
        for entries in registry.values() {
            for entry in entries.iter().filter(|e| e.is_public) {
                let (longest, total) = if let (Some(g), Some(branches)) = (&graph, &branches) {
                    g.density_of(entry, branches)
                } else {
                    let mut visited: HashSet<*const FnInfo> = HashSet::new();
                    walk_density(entry, &registry, &mut visited)
                };
                let path_breach = longest > cfg.warn_path;
                let total_breach = total > cfg.warn_total;
                if !path_breach && !total_breach {
                    continue;
                }
                let (rel, line, col) = &entry.span;
                let key = format!("{}:{}:{}", rel, line, col);
                let msg = format!(
                    "`{}` reachable branch density is {longest} along the deepest execution path \
                     and {total} across the whole call graph. Cumulative branches stress the \
                     branch predictor regardless of how the if/else statements are split across \
                     helper functions — extracting them into helpers does not reduce the number \
                     of conditional jumps the CPU sees. Consider caching predicate state \
                     (compute once when it changes, read once on the hot path), eliminating \
                     redundant work, or using a tag-enum + match instead of branch ladders.",
                    fn_name(&entry.key)
                );
                violations.push(Violation::warn(ID, key, msg));
            }
        }
        violations.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(violations)
    }
}

/// The real workspace call graph loaded from `callgraph.json`, indexed so an AST
/// `FnInfo` can be joined to its node and the branch density walked along the
/// real direct-call edges.
struct RealGraph {
    /// `node -> direct callee nodes`.
    edges: HashMap<NodeKey, Vec<NodeKey>>,
    /// `(rel_file, base_name) -> candidate node lines`, for the source-join.
    nodes_by_name: HashMap<(String, String), Vec<usize>>,
}

impl RealGraph {
    /// Parse `callgraph.json`. Missing / malformed file → `None` (silent
    /// fall back to the static resolver).
    fn load(path: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(path).ok()?;
        let doc: serde_json::Value = serde_json::from_str(&text).ok()?;
        let nodes = doc.get("nodes")?.as_object()?;
        let edges_obj = doc.get("edges")?.as_object()?;

        let mut nodes_by_name: HashMap<(String, String), Vec<usize>> = HashMap::new();
        for (key, name) in nodes {
            let (Some(file), Some(line), Some(name)) =
                (node_file(key), node_line(key), name.as_str())
            else {
                continue;
            };
            nodes_by_name
                .entry((file.to_string(), name.to_string()))
                .or_default()
                .push(line);
        }

        let mut edges: HashMap<NodeKey, Vec<NodeKey>> = HashMap::new();
        for (src, targets) in edges_obj {
            let Some(arr) = targets.as_array() else {
                continue;
            };
            let dsts = arr
                .iter()
                .filter_map(|t| t.as_str().map(str::to_string))
                .collect();
            edges.insert(src.clone(), dsts);
        }
        Some(Self {
            edges,
            nodes_by_name,
        })
    }

    /// Resolve `(rel_file, base_name)` to a node key, disambiguating multiple
    /// same-name candidates by the declaration line nearest the AST span line.
    /// Mirrors the phase-A source-join used by `cargo xtask callgraph`.
    fn join(&self, file: &str, name: &str, ast_line: usize) -> Option<NodeKey> {
        let lines = self
            .nodes_by_name
            .get(&(file.to_string(), name.to_string()))?;
        let line = lines
            .iter()
            .copied()
            .min_by_key(|line| line.abs_diff(ast_line))?;
        Some(format!("{file}:{line}"))
    }

    /// `node -> own_branches`, built by joining every AST `FnInfo` to its node.
    /// Nodes with no AST match keep an implicit 0 (closures / derive bodies).
    fn branches_by_node(&self, registry: &HashMap<FnKey, Vec<FnInfo>>) -> HashMap<NodeKey, usize> {
        let mut out: HashMap<NodeKey, usize> = HashMap::new();
        for entries in registry.values() {
            for info in entries {
                if let Some(node) = self.join(&info.span.0, fn_name(&info.key), info.span.1) {
                    // Same node line can host several same-name candidates; keep
                    // the heaviest so the density is not under-counted.
                    let slot = out.entry(node).or_insert(0);
                    *slot = (*slot).max(info.own_branches);
                }
            }
        }
        out
    }

    /// Branch density for one public entry, joining it to its graph node and
    /// walking the real edges. Entries with no graph node count their own
    /// branches only (e.g. a public fn the IR never emitted a node for).
    fn density_of(&self, entry: &FnInfo, branches: &HashMap<NodeKey, usize>) -> (usize, usize) {
        let Some(node) = self.join(&entry.span.0, fn_name(&entry.key), entry.span.1) else {
            return (entry.own_branches, entry.own_branches);
        };
        let mut visited: HashSet<NodeKey> = HashSet::new();
        walk_real(&node, &self.edges, branches, &mut visited)
    }
}

/// Load the checked-in call graph, returning `None` on any read/parse failure.
fn load_callgraph(path: &Path) -> Option<RealGraph> {
    RealGraph::load(path)
}

/// `"file:line"` → `file`. Splits on the LAST `:` so Windows-style drive
/// prefixes never appear here (paths are already `/`-normalised).
fn node_file(key: &str) -> Option<&str> {
    key.rsplit_once(':').map(|(file, _)| file)
}

/// `"file:line"` → `line`.
fn node_line(key: &str) -> Option<usize> {
    key.rsplit_once(':').and_then(|(_, line)| line.parse().ok())
}

/// DFS through the real call graph counting branches. Returns
/// `(longest_path_branches, total_branches)`, reproducing `walk_density`'s
/// accumulation contract over real direct-call edges: `longest = own + max
/// callee longest`, `total = own + sum callee totals`. The visited set of node
/// keys guards against cycles (recursion, mutual calls).
fn walk_real(
    node: &NodeKey,
    edges: &HashMap<NodeKey, Vec<NodeKey>>,
    branches: &HashMap<NodeKey, usize>,
    visited: &mut HashSet<NodeKey>,
) -> (usize, usize) {
    if !visited.insert(node.clone()) {
        return (0, 0);
    }
    let own = branches.get(node).copied().unwrap_or(0);
    let mut deepest_callee_path = 0;
    let mut callee_total = 0;
    if let Some(callees) = edges.get(node) {
        for callee in callees {
            let (path, total) = walk_real(callee, edges, branches, visited);
            if path > deepest_callee_path {
                deepest_callee_path = path;
            }
            callee_total += total;
        }
    }
    (own + deepest_callee_path, own + callee_total)
}

fn fn_name(key: &FnKey) -> &str {
    match key {
        FnKey::Free(name) | FnKey::Method { name, .. } => name,
    }
}

/// DFS through the call graph counting branches. Returns
/// `(longest_path_branches, total_branches)`. The cycle guard uses the
/// `FnInfo` pointer identity so that distinct definitions sharing a key
/// (rare cross-crate collisions) are still visited independently while a
/// recursive call back into the same definition terminates.
fn walk_density<'a>(
    entry: &'a FnInfo,
    registry: &'a HashMap<FnKey, Vec<FnInfo>>,
    visited: &mut HashSet<*const FnInfo>,
) -> (usize, usize) {
    if !visited.insert(entry as *const FnInfo) {
        return (0, 0);
    }
    let mut deepest_callee_path = 0;
    let mut callee_total = 0;
    for callee_key in &entry.calls {
        let Some(targets) = registry.get(callee_key) else {
            continue;
        };
        for target in targets {
            let (path, total) = walk_density(target, registry, visited);
            if path > deepest_callee_path {
                deepest_callee_path = path;
            }
            callee_total += total;
        }
    }
    (
        entry.own_branches + deepest_callee_path,
        entry.own_branches + callee_total,
    )
}

/// Walk a parsed file once and stash every function/method definition
/// into the registry keyed by `FnKey`.
fn collect_functions(file: &syn::File, rel: &str, registry: &mut HashMap<FnKey, Vec<FnInfo>>) {
    for item in &file.items {
        match item {
            Item::Fn(item_fn) => {
                let info = build_fn_info(
                    rel,
                    FnKey::Free(item_fn.sig.ident.to_string()),
                    None,
                    is_public_visibility(&item_fn.vis),
                    item_fn.span(),
                    &item_fn.block.stmts,
                );
                registry.entry(info.key.clone()).or_default().push(info);
            }
            Item::Impl(item_impl) => {
                walk_impl(rel, item_impl, registry);
            }
            Item::Mod(m) => {
                if let Some((_, items)) = &m.content {
                    let synthetic = syn::File {
                        shebang: None,
                        attrs: Vec::new(),
                        items: items.clone(),
                    };
                    collect_functions(&synthetic, rel, registry);
                }
            }
            _ => {}
        }
    }
}

fn walk_impl(rel: &str, item: &ItemImpl, registry: &mut HashMap<FnKey, Vec<FnInfo>>) {
    let trait_impl = item.trait_.is_some();
    let self_ty = self_type_name(&item.self_ty);
    for impl_item in &item.items {
        if let ImplItem::Fn(method) = impl_item {
            let is_public = trait_impl || is_public_visibility(&method.vis);
            let name = method.sig.ident.to_string();
            // Methods on a syntactically-known Self type get a Method key;
            // impls on non-path self types (e.g. `impl (A, B)`) fall back to
            // a Free key — sound because such methods are only ever reached
            // via UNRESOLVED method calls, so they are entry-only.
            let key = match &self_ty {
                Some(ty) => FnKey::Method {
                    ty: ty.clone(),
                    name,
                },
                None => FnKey::Free(name),
            };
            let info = build_fn_info(
                rel,
                key,
                self_ty.clone(),
                is_public,
                method.span(),
                &method.block.stmts,
            );
            registry.entry(info.key.clone()).or_default().push(info);
            for stmt in &method.block.stmts {
                if let Stmt::Item(Item::Mod(m)) = stmt
                    && let Some((_, items)) = &m.content
                {
                    let synthetic = syn::File {
                        shebang: None,
                        attrs: Vec::new(),
                        items: items.clone(),
                    };
                    collect_functions(&synthetic, rel, registry);
                }
            }
        }
    }
}

/// Last path-segment ident of a `Self` type, if it is a plain type path.
fn self_type_name(ty: &Type) -> Option<String> {
    if let Type::Path(tp) = ty {
        tp.path.segments.last().map(|s| s.ident.to_string())
    } else {
        None
    }
}

fn build_fn_info(
    rel: &str,
    key: FnKey,
    cur_self: Option<String>,
    is_public: bool,
    span: proc_macro2::Span,
    body: &[Stmt],
) -> FnInfo {
    let mut counter = BodyCounter {
        own_branches: 0,
        calls: Vec::new(),
        cur_self,
    };
    counter.visit_stmts(body);
    let s = span.start();
    FnInfo {
        own_branches: counter.own_branches,
        calls: counter.calls,
        span: (rel.to_string(), s.line, s.column),
        is_public,
        key,
    }
}

fn is_public_visibility(vis: &Visibility) -> bool {
    !matches!(vis, Visibility::Inherited)
}

struct BodyCounter {
    own_branches: usize,
    calls: Vec<FnKey>,
    /// The Self type of the enclosing impl, if any. Drives `self`/`Self`
    /// method-call resolution.
    cur_self: Option<String>,
}

impl BodyCounter {
    fn visit_stmts(&mut self, stmts: &[Stmt]) {
        for s in stmts {
            self.visit_stmt(s);
        }
    }

    /// Resolve a method-call receiver/method to a key, or `None` (UNRESOLVED).
    fn resolve_method_call(&self, node: &ExprMethodCall) -> Option<FnKey> {
        let name = node.method.to_string();
        if receiver_is_self(node.receiver.as_ref()) {
            self.cur_self.as_ref().map(|ty| FnKey::Method {
                ty: ty.clone(),
                name,
            })
        } else {
            None
        }
    }

    /// Resolve a path-function call to a key, or `None` (UNRESOLVED).
    fn resolve_path_call(&self, node: &ExprCall) -> Option<FnKey> {
        let Expr::Path(path) = node.func.as_ref() else {
            return None;
        };
        let segs = &path.path.segments;
        let last = segs.last()?.ident.to_string();
        if segs.len() >= 2 {
            let prev = segs[segs.len() - 2].ident.to_string();
            if prev == "Self" {
                return self.cur_self.as_ref().map(|ty| FnKey::Method {
                    ty: ty.clone(),
                    name: last,
                });
            }
            Some(FnKey::Method {
                ty: prev,
                name: last,
            })
        } else {
            Some(FnKey::Free(last))
        }
    }
}

fn receiver_is_self(expr: &Expr) -> bool {
    if let Expr::Path(path) = expr {
        path.qself.is_none()
            && path.path.segments.len() == 1
            && path
                .path
                .segments
                .first()
                .is_some_and(|s| s.ident == "self" || s.ident == "Self")
    } else {
        false
    }
}

impl<'ast> Visit<'ast> for BodyCounter {
    fn visit_expr_if(&mut self, node: &'ast ExprIf) {
        self.own_branches += 1;
        visit::visit_expr_if(self, node);
    }

    fn visit_expr_match(&mut self, node: &'ast ExprMatch) {
        let arms = node.arms.len();
        if arms > 1 {
            self.own_branches += arms - 1;
        }
        visit::visit_expr_match(self, node);
    }

    fn visit_expr_while(&mut self, node: &'ast syn::ExprWhile) {
        self.own_branches += 1;
        visit::visit_expr_while(self, node);
    }

    fn visit_expr_for_loop(&mut self, node: &'ast syn::ExprForLoop) {
        self.own_branches += 1;
        visit::visit_expr_for_loop(self, node);
    }

    fn visit_expr_loop(&mut self, node: &'ast syn::ExprLoop) {
        self.own_branches += 1;
        visit::visit_expr_loop(self, node);
    }

    fn visit_expr_try(&mut self, node: &'ast ExprTry) {
        self.own_branches += 1;
        visit::visit_expr_try(self, node);
    }

    fn visit_expr_binary(&mut self, node: &'ast ExprBinary) {
        if matches!(node.op, BinOp::And(_) | BinOp::Or(_)) {
            self.own_branches += 1;
        }
        visit::visit_expr_binary(self, node);
    }

    fn visit_local(&mut self, node: &'ast Local) {
        if let Some(init) = &node.init
            && init.diverge.is_some()
        {
            self.own_branches += 1;
        }
        visit::visit_local(self, node);
    }

    fn visit_expr_call(&mut self, node: &'ast ExprCall) {
        if let Some(key) = self.resolve_path_call(node) {
            self.calls.push(key);
        }
        visit::visit_expr_call(self, node);
    }

    fn visit_expr_method_call(&mut self, node: &'ast ExprMethodCall) {
        if let Some(key) = self.resolve_method_call(node) {
            self.calls.push(key);
        }
        visit::visit_expr_method_call(self, node);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count_body(body: &str) -> usize {
        let block: syn::Block = syn::parse_str(body).expect("valid Rust block");
        let mut counter = BodyCounter {
            own_branches: 0,
            calls: Vec::new(),
            cur_self: None,
        };
        counter.visit_stmts(&block.stmts);
        counter.own_branches
    }

    /// Count branches and collect resolved call keys for a body with an
    /// optional enclosing Self type.
    fn analyze(body: &str, cur_self: Option<&str>) -> (usize, Vec<FnKey>) {
        let block: syn::Block = syn::parse_str(body).expect("valid Rust block");
        let mut counter = BodyCounter {
            own_branches: 0,
            calls: Vec::new(),
            cur_self: cur_self.map(str::to_string),
        };
        counter.visit_stmts(&block.stmts);
        (counter.own_branches, counter.calls)
    }

    fn leaf(key: FnKey, own: usize) -> FnInfo {
        FnInfo {
            own_branches: own,
            calls: Vec::new(),
            span: ("x.rs".into(), 1, 0),
            is_public: true,
            key,
        }
    }

    #[test]
    fn counts_if_and_else_if() {
        let n = count_body(
            r#"{
                if a { 1 } else if b { 2 } else { 3 };
            }"#,
        );
        assert_eq!(n, 2);
    }

    #[test]
    fn counts_match_arms_minus_one() {
        let n = count_body(
            r#"{
                match x {
                    A => 1,
                    B => 2,
                    C => 3,
                };
            }"#,
        );
        assert_eq!(n, 2);
    }

    #[test]
    fn counts_let_else_question_mark_and_logical_ops() {
        let n = count_body(
            r#"{
                let Some(_a) = opt else { return; };
                if x && y { run()?; }
            }"#,
        );
        assert_eq!(n, 4);
    }

    #[test]
    fn collects_free_and_type_qualified_calls() {
        let (_, calls) = analyze(
            r#"{
                helper(a);
                module::path::call(c);
                Foo::bar(d);
            }"#,
            None,
        );
        assert!(calls.contains(&FnKey::Free("helper".into())));
        // `module::path::call` → second-to-last segment `path` treated as type.
        assert!(calls.contains(&FnKey::Method {
            ty: "path".into(),
            name: "call".into()
        }));
        assert!(calls.contains(&FnKey::Method {
            ty: "Foo".into(),
            name: "bar".into()
        }));
    }

    #[test]
    fn non_self_method_receiver_is_unresolved() {
        let (_, calls) = analyze(
            r#"{
                value.method(b);
            }"#,
            Some("Foo"),
        );
        // A non-self receiver carries no syntactic type → not traversable.
        assert!(calls.is_empty());
    }

    #[test]
    fn self_method_call_resolves_to_cur_self() {
        let (_, calls) = analyze(
            r#"{
                self.helper(b);
            }"#,
            Some("Foo"),
        );
        assert_eq!(
            calls,
            vec![FnKey::Method {
                ty: "Foo".into(),
                name: "helper".into()
            }]
        );
    }

    #[test]
    fn self_uppercase_path_call_resolves_to_cur_self() {
        let (_, calls) = analyze(
            r#"{
                let _x = Self::make();
            }"#,
            Some("Widget"),
        );
        assert_eq!(
            calls,
            vec![FnKey::Method {
                ty: "Widget".into(),
                name: "make".into()
            }]
        );
    }

    #[test]
    fn self_method_without_cur_self_is_unresolved() {
        // No enclosing impl → cannot resolve `self.x()`.
        let (_, calls) = analyze(
            r#"{
                self.helper(b);
            }"#,
            None,
        );
        assert!(calls.is_empty());
    }

    #[test]
    fn walk_resolves_type_aware_not_by_bare_name() {
        // Two methods named `new` on different types with different branch
        // counts. A caller of `Foo::new` must pick up Foo's count, not Bar's.
        let mut registry: HashMap<FnKey, Vec<FnInfo>> = HashMap::new();
        registry
            .entry(FnKey::Method {
                ty: "Foo".into(),
                name: "new".into(),
            })
            .or_default()
            .push(leaf(
                FnKey::Method {
                    ty: "Foo".into(),
                    name: "new".into(),
                },
                3,
            ));
        registry
            .entry(FnKey::Method {
                ty: "Bar".into(),
                name: "new".into(),
            })
            .or_default()
            .push(leaf(
                FnKey::Method {
                    ty: "Bar".into(),
                    name: "new".into(),
                },
                99,
            ));

        let caller = FnInfo {
            own_branches: 1,
            calls: vec![FnKey::Method {
                ty: "Foo".into(),
                name: "new".into(),
            }],
            span: ("x.rs".into(), 10, 0),
            is_public: true,
            key: FnKey::Free("caller".into()),
        };
        let mut visited = HashSet::new();
        let (longest, total) = walk_density(&caller, &registry, &mut visited);
        // 1 (own) + 3 (Foo::new), Bar::new (99) must be excluded.
        assert_eq!(longest, 4);
        assert_eq!(total, 4);
    }

    #[test]
    fn walk_excludes_unresolved_method_callee_branches() {
        // The callee's branches are excluded because the call site is an
        // unresolved non-self method call.
        let mut registry: HashMap<FnKey, Vec<FnInfo>> = HashMap::new();
        registry
            .entry(FnKey::Method {
                ty: "Heavy".into(),
                name: "method".into(),
            })
            .or_default()
            .push(leaf(
                FnKey::Method {
                    ty: "Heavy".into(),
                    name: "method".into(),
                },
                100,
            ));

        let (_, calls) = analyze(r#"{ value.method(b); }"#, Some("Foo"));
        let caller = FnInfo {
            own_branches: 2,
            calls,
            span: ("x.rs".into(), 1, 0),
            is_public: true,
            key: FnKey::Free("caller".into()),
        };
        let mut visited = HashSet::new();
        let (longest, total) = walk_density(&caller, &registry, &mut visited);
        assert_eq!(longest, 2);
        assert_eq!(total, 2);
    }

    fn edges(pairs: &[(&str, &[&str])]) -> HashMap<NodeKey, Vec<NodeKey>> {
        pairs
            .iter()
            .map(|(src, dsts)| {
                (
                    (*src).to_string(),
                    dsts.iter().map(|d| (*d).to_string()).collect(),
                )
            })
            .collect()
    }

    fn branches(pairs: &[(&str, usize)]) -> HashMap<NodeKey, usize> {
        pairs.iter().map(|(k, v)| ((*k).to_string(), *v)).collect()
    }

    #[test]
    fn real_walk_accumulates_longest_and_total_over_edges() {
        //        root(1)
        //        /     \
        //     a(2)     b(3)
        //      |        |
        //     c(10)    d(4)
        // longest path: root->a->c = 1 + 2 + 10 = 13
        // total: 1 + (2 + 10) + (3 + 4) = 20
        let edges = edges(&[
            ("f:1", &["f:2", "f:3"]),
            ("f:2", &["f:10"]),
            ("f:3", &["f:4"]),
        ]);
        let branches = branches(&[("f:1", 1), ("f:2", 2), ("f:3", 3), ("f:4", 4), ("f:10", 10)]);
        let mut visited = HashSet::new();
        let (longest, total) = walk_real(&"f:1".to_string(), &edges, &branches, &mut visited);
        assert_eq!(longest, 13);
        assert_eq!(total, 20);
    }

    #[test]
    fn real_walk_uses_zero_for_nodes_without_branches() {
        // `f:2` has no entry in branches_by_node (a closure/derive node).
        let edges = edges(&[("f:1", &["f:2"]), ("f:2", &["f:3"])]);
        let branches = branches(&[("f:1", 5), ("f:3", 7)]);
        let mut visited = HashSet::new();
        let (longest, total) = walk_real(&"f:1".to_string(), &edges, &branches, &mut visited);
        // 5 + 0 (f:2) + 7 (f:3) along the only path; total identical (linear).
        assert_eq!(longest, 12);
        assert_eq!(total, 12);
    }

    #[test]
    fn real_walk_cycle_guard_terminates() {
        // Direct recursion plus a mutual cycle must both terminate without
        // double-counting the re-entered node.
        let edges = edges(&[("f:1", &["f:1", "f:2"]), ("f:2", &["f:1"])]);
        let branches = branches(&[("f:1", 4), ("f:2", 6)]);
        let mut visited = HashSet::new();
        let (longest, total) = walk_real(&"f:1".to_string(), &edges, &branches, &mut visited);
        // f:1 visited once (own 4); its self-edge re-enters → (0,0); f:2 (own 6)
        // re-enters f:1 → (0,0). longest = 4 + max(0, 6) = 10; total = 4 + 0 + 6 = 10.
        assert_eq!(longest, 10);
        assert_eq!(total, 10);
    }
}

//! Cumulative branch density per public entry point.
//!
//! `branch_chains` and `guard_cascade` flag long sequences inside a single
//! block. They miss a class of equally branch-heavy code: a public
//! method whose body is "clean" but which calls helpers that each carry
//! their own ifs and matches. The CPU's branch predictor sees the
//! cumulative count along whatever execution path runs, not the syntactic
//! structure of one source file. Extracting `if`s into a helper does not
//! reduce the number of conditional jumps observed at runtime —
//! deceptive when the helper is `#[inline]` and inlined back, and still
//! wasteful when it isn't (`ICache` misses on the call hop count too).
//!
//! This check counts branches **transitively** from each public entry
//! point. For every `pub fn` (or `pub(crate)` item that escapes the
//! module via re-export), we walk the call graph by name match,
//! accumulating two metrics:
//!
//! - **`longest_path_branches`** — the worst-case number of conditional
//!   jumps a single execution path can hit, as the sum of branches in
//!   every function on the deepest reached chain.
//! - **`total_branches`** — the sum of branches across every reachable
//!   function. Catches public APIs whose call graph is broad rather
//!   than deep (many helpers, each with a few branches each).
//!
//! Both metrics use the same per-function branch counter: each `if`,
//! `else if`, `if let`, `let-else`, every additional `match` arm beyond
//! the first, every loop back-edge (`loop`/`while`/`for`), every `?`,
//! and every short-circuit `&&`/`||` adds one.
//!
//! Call resolution is best-effort by name (last path segment for
//! `foo::bar::baz()`, method name for `x.method()`). Cross-crate calls,
//! trait dynamic dispatch, and macro expansions count as "external" and
//! terminate the walk; they will be undercounted, never overcounted.
//! Cycles are broken by visited-tracking per starting entry.

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use syn::{
    BinOp, Expr, ExprBinary, ExprCall, ExprIf, ExprMatch, ExprMethodCall, ExprTry, ImplItem, Item,
    ItemImpl, Local, Stmt, Visibility,
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

pub(crate) struct FunctionBranchDensity;

#[derive(Debug, Clone)]
struct FnInfo {
    /// Branches in *this* function's body (no descent).
    own_branches: usize,
    /// Names of called functions (last path segment / method ident).
    calls: Vec<String>,
    /// `(rel_path, line, col)` for the function declaration.
    span: (String, usize, usize),
    /// `pub` / `pub(crate)` / `pub(super)` etc. — only public entries
    /// drive the walk.
    is_public: bool,
    /// Function name (used as registry key; collisions across the
    /// workspace are conservative — both implementations get walked,
    /// inflating but never deflating the metric).
    name: String,
}

impl Check for FunctionBranchDensity {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.function_branch_density;
        let exempt = compile_globs(&cfg.exempt_files);
        let mut registry: HashMap<String, Vec<FnInfo>> = HashMap::new();
        for path in workspace_rs_files_scoped(ctx.workspace_root, ctx.scope)? {
            let rel_path = relative_to(ctx.workspace_root, &path).to_path_buf();
            let rel = rel_path.to_string_lossy().replace('\\', "/");
            if matches_any(&exempt, std::path::Path::new(&rel)) {
                continue;
            }
            let Ok(file) = parse_file(&path) else {
                continue;
            };
            collect_functions(&file, &rel, &mut registry);
        }

        let mut violations = Vec::new();
        for entries in registry.values() {
            for entry in entries.iter().filter(|e| e.is_public) {
                let mut visited: HashSet<&str> = HashSet::new();
                let (longest, total) = walk_density(entry, &registry, &mut visited);
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
                    entry.name
                );
                violations.push(Violation::warn(ID, key, msg));
            }
        }
        violations.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(violations)
    }
}

/// DFS through the call graph counting branches. Returns
/// `(longest_path_branches, total_branches)`.
fn walk_density<'a>(
    entry: &'a FnInfo,
    registry: &'a HashMap<String, Vec<FnInfo>>,
    visited: &mut HashSet<&'a str>,
) -> (usize, usize) {
    if !visited.insert(entry.name.as_str()) {
        return (0, 0);
    }
    let mut deepest_callee_path = 0;
    let mut callee_total = 0;
    for callee_name in &entry.calls {
        let Some(targets) = registry.get(callee_name) else {
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
/// into the registry keyed by name.
fn collect_functions(file: &syn::File, rel: &str, registry: &mut HashMap<String, Vec<FnInfo>>) {
    for item in &file.items {
        match item {
            Item::Fn(item_fn) => {
                let info = build_fn_info(
                    rel,
                    &item_fn.sig.ident.to_string(),
                    is_public_visibility(&item_fn.vis),
                    item_fn.span(),
                    &item_fn.block.stmts,
                );
                registry.entry(info.name.clone()).or_default().push(info);
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

fn walk_impl(rel: &str, item: &ItemImpl, registry: &mut HashMap<String, Vec<FnInfo>>) {
    let trait_impl = item.trait_.is_some();
    for impl_item in &item.items {
        if let ImplItem::Fn(method) = impl_item {
            let is_public = trait_impl || is_public_visibility(&method.vis);
            let info = build_fn_info(
                rel,
                &method.sig.ident.to_string(),
                is_public,
                method.span(),
                &method.block.stmts,
            );
            registry.entry(info.name.clone()).or_default().push(info);
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

fn build_fn_info(
    rel: &str,
    name: &str,
    is_public: bool,
    span: proc_macro2::Span,
    body: &[Stmt],
) -> FnInfo {
    let mut counter = BodyCounter {
        own_branches: 0,
        calls: Vec::new(),
    };
    counter.visit_stmts(body);
    let s = span.start();
    FnInfo {
        own_branches: counter.own_branches,
        calls: counter.calls,
        span: (rel.to_string(), s.line, s.column),
        is_public,
        name: name.to_string(),
    }
}

fn is_public_visibility(vis: &Visibility) -> bool {
    !matches!(vis, Visibility::Inherited)
}

struct BodyCounter {
    own_branches: usize,
    calls: Vec<String>,
}

impl BodyCounter {
    fn visit_stmts(&mut self, stmts: &[Stmt]) {
        for s in stmts {
            self.visit_stmt(s);
        }
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
        if let Expr::Path(path) = node.func.as_ref()
            && let Some(seg) = path.path.segments.last()
        {
            self.calls.push(seg.ident.to_string());
        }
        visit::visit_expr_call(self, node);
    }

    fn visit_expr_method_call(&mut self, node: &'ast ExprMethodCall) {
        self.calls.push(node.method.to_string());
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
        };
        counter.visit_stmts(&block.stmts);
        counter.own_branches
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
    fn collects_call_targets() {
        let block: syn::Block = syn::parse_str(
            r#"{
                helper(a);
                obj.method(b);
                module::path::call(c);
            }"#,
        )
        .unwrap();
        let mut counter = BodyCounter {
            own_branches: 0,
            calls: Vec::new(),
        };
        counter.visit_stmts(&block.stmts);
        assert!(counter.calls.contains(&"helper".to_string()));
        assert!(counter.calls.contains(&"method".to_string()));
        assert!(counter.calls.contains(&"call".to_string()));
    }
}

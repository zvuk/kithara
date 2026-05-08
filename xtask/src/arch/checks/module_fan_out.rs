//! Per-module intra-crate import fan-out.
//!
//! Counts how many *distinct sibling modules of the same crate* each file
//! imports from. A file with high fan-out is reaching across the crate to
//! pull bits from many neighbours — the architectural symptom is "this
//! module is the orchestrator", which usually means responsibilities have
//! drifted into one place that should have been split.
//!
//! Detection is import-graph (not call-graph): we look at `use` items at
//! file scope and resolve each path to a top-level module of the same
//! crate, then count unique neighbours. Resolution rules:
//!
//!   * `use crate::A::B::…`        → top-level `A`
//!   * `use super::A::B::…`        → parent module's `A` (one hop up)
//!   * `use super::super::A::…`    → two hops up, then `A`
//!   * `use self::A::…`            → inside self, ignored
//!   * `use std::…` / external     → ignored (intra-crate only)
//!
//! Each *file* contributes one count, and self-imports of children of the
//! file's own module are subtracted (a `mod foo; use foo::Bar;` pattern
//! does not inflate the fan-out of the parent).
//!
//! Per-crate overrides (`[module_fan_out.overrides]`) let app/test/macro
//! crates relax the default without baselining each file. The threshold
//! is intentionally permissive: this check is a *map* of orchestrator
//! files, not a hard gate.

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use anyhow::Result;
use syn::{Item, UseTree};

use super::{Check, Context};
use crate::common::{
    parse::parse_file,
    violation::Violation,
    walker::{relative_to, workspace_rs_files_scoped},
};

pub(crate) const ID: &str = "module_fan_out";

pub(crate) struct ModuleFanOut;

impl Check for ModuleFanOut {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.thresholds.module_fan_out;
        let mut violations = Vec::new();

        for path in workspace_rs_files_scoped(ctx.workspace_root, ctx.scope)? {
            let Ok(file) = parse_file(&path) else {
                continue;
            };
            let rel = relative_to(ctx.workspace_root, &path);
            let key = rel.to_string_lossy().replace('\\', "/");
            let crate_name = crate_name_from_path(rel);
            let warn = crate_name
                .as_deref()
                .and_then(|c| cfg.overrides.get(c).copied())
                .unwrap_or(cfg.warn);

            let module_path = file_module_path(rel);
            let mut neighbours: BTreeSet<String> = BTreeSet::new();
            for item in &file.items {
                if let Item::Use(u) = item {
                    collect_use_targets(&u.tree, 0, &module_path, &mut neighbours);
                }
            }
            for child in &module_path {
                neighbours.remove(child);
            }

            let n = neighbours.len();
            if n >= warn {
                let msg = format!(
                    "{n} sibling-module imports from same crate (warn threshold {warn}); \
                     this file is acting as an orchestrator — split or push the wiring up"
                );
                violations.push(Violation::warn(ID, key, msg));
            }
        }
        Ok(violations)
    }
}

fn crate_name_from_path(rel: &Path) -> Option<String> {
    let mut comps = rel.components();
    if comps.next()?.as_os_str() != "crates" {
        return None;
    }
    Some(comps.next()?.as_os_str().to_str()?.to_string())
}

/// Module-path of a file relative to the crate root, as a vector of
/// component names. Examples (under `crates/<X>/`):
///   * `src/lib.rs`              → `[]`
///   * `src/foo.rs`              → `["foo"]`
///   * `src/foo/mod.rs`          → `["foo"]`
///   * `src/foo/bar.rs`          → `["foo", "bar"]`
///   * `src/foo/bar/baz.rs`      → `["foo", "bar", "baz"]`
fn file_module_path(rel: &Path) -> Vec<String> {
    let mut comps = rel.components();
    if comps.next().map(std::path::Component::as_os_str) != Some("crates".as_ref()) {
        return Vec::new();
    }
    let _crate = comps.next();
    if comps.next().map(std::path::Component::as_os_str) != Some("src".as_ref()) {
        return Vec::new();
    }
    let remainder: PathBuf = comps.collect();
    let mut out: Vec<String> = remainder
        .iter()
        .map(|c| c.to_string_lossy().to_string())
        .collect();
    if let Some(last) = out.last_mut() {
        if matches!(last.as_str(), "lib.rs" | "main.rs" | "mod.rs") {
            out.pop();
        } else if let Some(stem) = last.strip_suffix(".rs") {
            *last = stem.to_string();
        }
    }
    out
}

fn collect_use_targets(
    tree: &UseTree,
    depth: usize,
    module_path: &[String],
    out: &mut BTreeSet<String>,
) {
    match tree {
        UseTree::Path(p) => {
            let head = p.ident.to_string();
            if depth == 0 {
                match head.as_str() {
                    "crate" => collect_after_anchor(&p.tree, &[], out),
                    "super" => {
                        let mut cur: &UseTree = &p.tree;
                        let mut hops: usize = 1;
                        while let UseTree::Path(inner) = cur {
                            if inner.ident == "super" {
                                hops += 1;
                                cur = &inner.tree;
                            } else {
                                break;
                            }
                        }
                        let parent_depth = module_path.len().saturating_sub(hops);
                        let anchor = &module_path[..parent_depth];
                        collect_after_anchor(cur, anchor, out);
                    }
                    _ => {}
                }
            } else {
                collect_use_targets(&p.tree, depth + 1, module_path, out);
            }
        }
        UseTree::Group(g) => {
            for item in &g.items {
                collect_use_targets(item, depth, module_path, out);
            }
        }
        UseTree::Glob(_) | UseTree::Name(_) | UseTree::Rename(_) => {}
    }
}

/// After we've established that the use path is rooted at `crate::` or
/// resolved through `super::`, the next path segment is the neighbour
/// module name. `anchor` is the resolved-so-far module path; the first
/// `Path` we see after this becomes the neighbour.
fn collect_after_anchor(tree: &UseTree, anchor: &[String], out: &mut BTreeSet<String>) {
    match tree {
        UseTree::Path(p) => {
            let mut full = anchor.to_vec();
            full.push(p.ident.to_string());
            out.insert(full.join("::"));
        }
        UseTree::Group(g) => {
            for item in &g.items {
                collect_after_anchor(item, anchor, out);
            }
        }
        UseTree::Glob(_) | UseTree::Name(_) | UseTree::Rename(_) => {}
    }
}

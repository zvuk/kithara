//! Intra-crate layered architecture: `use crate::*` references must respect layers.
//!
//! Each `[[crate]]` entry in `module-layers.toml` declares a list of layers
//! with file globs. A file in layer N is allowed to depend only on files in
//! layers ≤ N. Crates without a `[[crate]]` entry are skipped.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use anyhow::Result;
use cargo_metadata::Package;
use glob::Pattern;
use syn::{Item, UseTree};

use super::{Check, Context};
use crate::common::{violation::Violation, walker::walk_rs_files};

pub(crate) const ID: &str = "module_layers";

pub(crate) struct ModuleLayers;

impl Check for ModuleLayers {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
        let cfg = &ctx.config.module_layers;
        if cfg.crates.is_empty() {
            return Ok(Vec::new());
        }

        let pkgs: HashMap<&str, &Package> = ctx
            .metadata
            .workspace_packages()
            .iter()
            .map(|p| (p.name.as_str(), *p))
            .collect();

        let mut violations = Vec::new();

        for crate_cfg in &cfg.crates {
            let Some(pkg) = pkgs.get(crate_cfg.name.as_str()) else {
                continue;
            };
            let Some(manifest_dir) = pkg.manifest_path.parent() else {
                continue;
            };
            let crate_root = manifest_dir.as_std_path();

            // compile per-layer globs (relative to crate root)
            let layers: Vec<(u32, &str, Vec<Pattern>)> = crate_cfg
                .layers
                .iter()
                .map(|l| {
                    let pats = l
                        .paths
                        .iter()
                        .filter_map(|p| Pattern::new(p).ok())
                        .collect();
                    (l.index, l.name.as_str(), pats)
                })
                .collect();

            // classify every src/**/*.rs into a layer
            let src_dir = crate_root.join("src");
            let rs_files = walk_rs_files(&src_dir)?;
            let file_layer: HashMap<PathBuf, (u32, &str)> = rs_files
                .iter()
                .filter_map(|f| {
                    let rel = f.strip_prefix(crate_root).ok()?;
                    let s = rel.to_string_lossy().replace('\\', "/");
                    layers
                        .iter()
                        .find(|(_, _, pats)| pats.iter().any(|p| p.matches(&s)))
                        .map(|(idx, name, _)| (f.clone(), (*idx, *name)))
                })
                .collect();

            for (path, &(from_idx, from_name)) in &file_layer {
                let Ok(parsed) = syn::parse_file(&std::fs::read_to_string(path)?) else {
                    continue;
                };
                for use_path in collect_crate_use_paths(&parsed.items) {
                    let Some(target) = resolve_module_to_file(crate_root, &use_path) else {
                        continue;
                    };
                    let Some(&(to_idx, to_name)) = file_layer.get(&target) else {
                        continue;
                    };
                    if to_idx > from_idx {
                        let from_rel = path
                            .strip_prefix(crate_root)
                            .unwrap_or(path)
                            .to_string_lossy();
                        let key = format!(
                            "{}::{from_rel} -> crate::{}",
                            crate_cfg.name,
                            use_path.join("::"),
                        );
                        let msg = format!(
                            "file in layer {from_idx} '{from_name}' depends on file in \
                             layer {to_idx} '{to_name}', which is higher"
                        );
                        violations.push(Violation::deny(ID, key, msg));
                    }
                }
            }
        }
        Ok(violations)
    }
}

/// Collect `use crate::a::b::...` segment lists from items recursively.
fn collect_crate_use_paths(items: &[Item]) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    for item in items {
        match item {
            Item::Use(u) => collect_use_tree(&u.tree, &mut Vec::new(), &mut out, false),
            Item::Mod(m) => {
                if let Some((_, items)) = &m.content {
                    out.extend(collect_crate_use_paths(items));
                }
            }
            _ => {}
        }
    }
    out
}

fn collect_use_tree(
    tree: &UseTree,
    prefix: &mut Vec<String>,
    out: &mut Vec<Vec<String>>,
    inside_crate: bool,
) {
    match tree {
        UseTree::Path(p) => {
            let seg = p.ident.to_string();
            let now_inside = inside_crate || seg == "crate";
            if !inside_crate && seg != "crate" {
                return;
            }
            if seg != "crate" {
                prefix.push(seg);
            }
            collect_use_tree(&p.tree, prefix, out, now_inside);
            if !prefix.is_empty() && now_inside && tree_is_terminal(&p.tree) {
                // already pushed via terminal branch
            }
            if !inside_crate {
                // we entered via `crate::` here; pop nothing (we did not push "crate")
            }
        }
        UseTree::Name(n) => {
            if inside_crate {
                let mut path = prefix.clone();
                path.push(n.ident.to_string());
                out.push(path);
            }
        }
        UseTree::Rename(r) => {
            if inside_crate {
                let mut path = prefix.clone();
                path.push(r.ident.to_string());
                out.push(path);
            }
        }
        UseTree::Glob(_) => {
            if inside_crate {
                out.push(prefix.clone());
            }
        }
        UseTree::Group(g) => {
            for item in &g.items {
                collect_use_tree(item, &mut prefix.clone(), out, inside_crate);
            }
        }
    }
}

fn tree_is_terminal(t: &UseTree) -> bool {
    matches!(
        t,
        UseTree::Name(_) | UseTree::Rename(_) | UseTree::Glob(_) | UseTree::Group(_)
    )
}

/// Try to map a `crate::a::b::c` path to a `.rs` file under `<crate_root>/src/`.
/// Tries `src/a/b/c.rs`, `src/a/b/c/mod.rs`, `src/a/b.rs` (the last segment may be an item).
fn resolve_module_to_file(crate_root: &Path, segments: &[String]) -> Option<PathBuf> {
    if segments.is_empty() {
        return None;
    }
    let src = crate_root.join("src");
    let mut try_paths: Vec<PathBuf> = Vec::new();
    // exact module match
    let mut exact = src.clone();
    for seg in segments {
        exact = exact.join(seg);
    }
    try_paths.push(exact.with_extension("rs"));
    try_paths.push(exact.join("mod.rs"));
    // last segment is an item inside the parent module
    if segments.len() >= 2 {
        let mut parent = src.clone();
        for seg in &segments[..segments.len() - 1] {
            parent = parent.join(seg);
        }
        try_paths.push(parent.with_extension("rs"));
        try_paths.push(parent.join("mod.rs"));
    }
    try_paths.into_iter().find(|p| p.is_file())
}

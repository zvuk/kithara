use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result};
use glob::Pattern;
use syn::{
    Attribute, Expr, Item, Lit, Meta,
    punctuated::Punctuated,
    visit::{self, Visit},
};

use super::{attrs_are_test_only, item_is_test_only};
use crate::common::walker::walk_rs_files;

struct ModuleFile {
    children: Vec<(PathBuf, bool)>,
    root_children: Vec<(PathBuf, bool)>,
    test_only: bool,
}

/// Follow Cargo target declarations without reading excluded or unrelated files.
pub(crate) fn module_files(
    target: &Path,
    excluded: impl Fn(&Path) -> bool,
) -> Result<Vec<PathBuf>> {
    let target =
        fs::canonicalize(target).with_context(|| format!("resolve target {}", target.display()))?;
    let mut pending = vec![(target, true)];
    let mut reached = BTreeSet::new();
    while let Some((path, root)) = pending.pop() {
        if excluded(&path) || !reached.insert(path.clone()) {
            continue;
        }
        let source =
            fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let file = syn::parse_file(&source).with_context(|| format!("parse {}", path.display()))?;
        let Some(parent) = path.parent() else {
            continue;
        };
        let Some(directory) = (if root {
            Some(parent.to_path_buf())
        } else {
            module_dir(&path)
        }) else {
            continue;
        };
        let mut children = Vec::new();
        let mut uncertain = BTreeSet::new();
        collect_children(
            &file.items,
            &directory,
            parent,
            false,
            false,
            &mut children,
            &mut uncertain,
        );
        pending.extend(children.into_iter().map(|(child, _)| (child, false)));
        pending.extend(uncertain.into_iter().map(|child| (child, false)));
    }
    Ok(reached.into_iter().collect())
}

/// Exact existing files reachable only through test-only module declarations.
/// Production reuse of a physical file keeps that file and its children checked.
/// Ambiguous, malformed, or unavailable source context stays in scope.
#[must_use]
pub fn cfg_test_module_globs(workspace_root: &Path) -> Vec<String> {
    let Ok(root) = fs::canonicalize(workspace_root) else {
        return Vec::new();
    };
    let Ok(files) = walk_rs_files(&root.join("crates")) else {
        return Vec::new();
    };
    let (roots, tainted_crates) = cargo_roots(&root, &files);
    module_globs(&root, files, &roots, tainted_crates)
}

/// Cargo-backed callers include custom and root-package target layouts.
pub(crate) fn cfg_test_module_globs_for_targets<'a>(
    workspace_root: &Path,
    targets: impl IntoIterator<Item = &'a Path>,
) -> Vec<String> {
    let Ok(root) = fs::canonicalize(workspace_root) else {
        return Vec::new();
    };
    let mut roots = BTreeSet::new();
    let mut files = BTreeSet::new();
    for target in targets {
        let Ok(target) = fs::canonicalize(target) else {
            return Vec::new();
        };
        let Ok(reachable) = module_files(&target, |_| false) else {
            return Vec::new();
        };
        roots.insert(target);
        files.extend(reachable);
    }
    module_globs(&root, files.into_iter().collect(), &roots, BTreeSet::new())
}

fn module_globs(
    root: &Path,
    files: Vec<PathBuf>,
    roots: &BTreeSet<PathBuf>,
    mut tainted_crates: BTreeSet<PathBuf>,
) -> Vec<String> {
    let mut graph = BTreeMap::new();
    let mut uncertain_roots = BTreeSet::new();
    for path in files {
        let Ok(path) = fs::canonicalize(&path) else {
            if let Some(directory) = crate_directory(root, &path) {
                tainted_crates.insert(directory);
            }
            continue;
        };
        let Ok(source) = fs::read_to_string(&path) else {
            if let Some(directory) = crate_directory(root, &path) {
                tainted_crates.insert(directory);
            }
            continue;
        };
        let Ok(file) = syn::parse_file(&source) else {
            if let Some(directory) = crate_directory(root, &path) {
                tainted_crates.insert(directory);
            }
            continue;
        };
        let Some(directory) = module_dir(&path) else {
            continue;
        };
        let Some(parent) = path.parent() else {
            continue;
        };
        let mut children = Vec::new();
        collect_children(
            &file.items,
            &directory,
            parent,
            false,
            false,
            &mut children,
            &mut uncertain_roots,
        );
        let root_children = if roots.contains(&path) && directory != parent {
            let mut children = Vec::new();
            collect_children(
                &file.items,
                parent,
                parent,
                false,
                false,
                &mut children,
                &mut uncertain_roots,
            );
            children
        } else {
            children.clone()
        };
        graph.insert(
            path,
            ModuleFile {
                children,
                root_children,
                test_only: attrs_are_test_only(&file.attrs),
            },
        );
    }
    let incoming: BTreeSet<_> = graph
        .values()
        .flat_map(|file| {
            file.children
                .iter()
                .chain(&file.root_children)
                .map(|(path, _)| path.clone())
        })
        .collect();
    let mut pending: Vec<_> = graph
        .keys()
        .filter(|path| roots.contains(*path) || !incoming.contains(*path))
        .map(|path| (path.clone(), false, roots.contains(path)))
        .collect();
    pending.extend(uncertain_roots.into_iter().map(|path| (path, false, false)));
    let mut reached = BTreeSet::new();
    let mut excluded = BTreeMap::new();
    while let Some((path, inherited_test, crate_root)) = pending.pop() {
        let Some(file) = graph.get(&path) else {
            continue;
        };
        let test_only = inherited_test || file.test_only;
        if !reached.insert((path.clone(), test_only, crate_root)) {
            continue;
        }
        excluded
            .entry(path)
            .and_modify(|value| *value &= test_only)
            .or_insert(test_only);
        let children = if crate_root {
            &file.root_children
        } else {
            &file.children
        };
        pending.extend(
            children
                .iter()
                .map(|(path, child_test)| (path.clone(), test_only || *child_test, false)),
        );
    }
    let mut globs: Vec<_> = excluded
        .iter()
        .filter(|(_, test_only)| **test_only)
        .filter(|(path, _)| {
            !tainted_crates
                .iter()
                .any(|directory| path.starts_with(directory))
        })
        .filter_map(|(path, _)| path.strip_prefix(root).ok())
        .map(|path| Pattern::escape(&path.to_string_lossy()))
        .collect();
    globs.sort();
    globs
}

/// The directory used by ordinary external module declarations in a file.
fn module_dir(file: &Path) -> Option<PathBuf> {
    let parent = file.parent()?;
    let stem = file.file_stem()?.to_str()?;
    Some(match stem {
        "mod" => parent.to_owned(),
        _ => parent.join(stem),
    })
}

fn collect_children(
    items: &[Item],
    directory: &Path,
    path_base: &Path,
    inherited_test: bool,
    inherited_uncertainty: bool,
    out: &mut Vec<(PathBuf, bool)>,
    uncertain_roots: &mut BTreeSet<PathBuf>,
) {
    for item in items {
        let Item::Mod(module) = item else {
            if !inherited_test && !item_is_test_only(item) {
                LocalModules {
                    uncertain_roots,
                    directory,
                    path_base,
                }
                .visit_item(item);
            }
            continue;
        };
        let (explicit, conditional) = module_paths(&module.attrs);
        let uncertain = inherited_uncertainty || conditional;
        let test_only = inherited_test || attrs_are_test_only(&module.attrs);
        if let Some((_, items)) = &module.content {
            let mut directories: BTreeSet<_> =
                explicit.iter().map(|path| path_base.join(path)).collect();
            if directories.is_empty() || conditional {
                directories.insert(directory.join(module.ident.to_string()));
            }
            for child_dir in directories {
                collect_children(
                    items,
                    &child_dir,
                    &child_dir,
                    test_only,
                    uncertain,
                    out,
                    uncertain_roots,
                );
            }
            continue;
        }
        let mut paths: BTreeSet<_> = explicit.iter().map(|path| path_base.join(path)).collect();
        if paths.is_empty() || conditional {
            let flat = directory.join(format!("{}.rs", module.ident));
            let nested = directory.join(module.ident.to_string()).join("mod.rs");
            for path in [flat, nested] {
                if path.is_file() {
                    paths.insert(path);
                }
            }
        }
        let uncertain = uncertain || paths.len() > 1;
        for path in paths {
            if let Ok(path) = fs::canonicalize(path) {
                if uncertain {
                    uncertain_roots.insert(path.clone());
                }
                out.push((path, test_only));
            }
        }
    }
}

struct LocalModules<'a> {
    uncertain_roots: &'a mut BTreeSet<PathBuf>,
    directory: &'a Path,
    path_base: &'a Path,
}

impl<'ast> Visit<'ast> for LocalModules<'_> {
    fn visit_item(&mut self, item: &'ast Item) {
        if item_is_test_only(item) {
            return;
        }
        if matches!(item, Item::Mod(_)) {
            collect_children(
                std::slice::from_ref(item),
                self.directory,
                self.path_base,
                false,
                true,
                &mut Vec::new(),
                self.uncertain_roots,
            );
        } else {
            visit::visit_item(self, item);
        }
    }
}

/// All concrete candidates are kept when source selection is uncertain.
fn module_paths(attrs: &[Attribute]) -> (Vec<PathBuf>, bool) {
    let mut paths = Vec::new();
    let mut conditional = false;
    for attr in attrs {
        if attr.path().is_ident("path") {
            if !paths.is_empty() {
                conditional = true;
            }
            collect_path(&attr.meta, &mut paths, &mut conditional);
        }
        collect_conditional_paths(&attr.meta, &mut paths, &mut conditional);
    }
    (paths, conditional)
}

fn collect_path(meta: &Meta, paths: &mut Vec<PathBuf>, uncertain: &mut bool) {
    if let Meta::NameValue(value) = meta
        && let Expr::Lit(value) = &value.value
        && let Lit::Str(value) = &value.lit
    {
        paths.push(PathBuf::from(value.value()));
    } else {
        *uncertain = true;
    }
}

fn collect_conditional_paths(meta: &Meta, paths: &mut Vec<PathBuf>, uncertain: &mut bool) {
    let Meta::List(list) = meta else {
        return;
    };
    if !list.path.is_ident("cfg_attr") {
        return;
    }
    let Ok(nested) = list.parse_args_with(Punctuated::<Meta, syn::Token![,]>::parse_terminated)
    else {
        *uncertain = true;
        return;
    };
    for meta in nested.iter().skip(1) {
        if meta.path().is_ident("path") {
            *uncertain = true;
            collect_path(meta, paths, uncertain);
        }
        collect_conditional_paths(meta, paths, uncertain);
    }
}

/// Cargo target paths are independent roots, even if a test module reuses them.
fn cargo_roots(root: &Path, files: &[PathBuf]) -> (BTreeSet<PathBuf>, BTreeSet<PathBuf>) {
    let crates = root.join("crates");
    let directories: BTreeSet<_> = files
        .iter()
        .filter_map(|file| file.strip_prefix(&crates).ok()?.components().next())
        .map(|component| crates.join(component.as_os_str()))
        .collect();
    let mut roots = BTreeSet::new();
    let mut tainted = BTreeSet::new();
    for directory in directories {
        let source_files: Vec<_> = files
            .iter()
            .filter(|path| path.starts_with(&directory))
            .collect();
        for file in &source_files {
            if file.strip_prefix(&directory).is_ok_and(is_automatic_target)
                && let Ok(path) = fs::canonicalize(file)
            {
                roots.insert(path);
            }
        }
        let manifest = directory.join("Cargo.toml");
        let Ok(source) = fs::read_to_string(&manifest) else {
            if manifest.exists() {
                tainted.insert(directory);
            }
            continue;
        };
        if let Some(paths) = manifest_target_paths(&source) {
            roots.extend(
                paths
                    .into_iter()
                    .filter_map(|path| fs::canonicalize(directory.join(path)).ok()),
            );
        } else {
            tainted.insert(directory);
        }
    }
    (roots, tainted)
}

fn crate_directory(root: &Path, file: &Path) -> Option<PathBuf> {
    let crates = root.join("crates");
    let name = file.strip_prefix(&crates).ok()?.components().next()?;
    Some(crates.join(name.as_os_str()))
}

fn manifest_target_paths(source: &str) -> Option<Vec<PathBuf>> {
    let manifest: toml::Value = toml::from_str(source).ok()?;
    let mut targets = Vec::new();
    if let Some(lib) = manifest.get("lib") {
        targets.push(lib);
    }
    for kind in ["bin", "example", "test", "bench"] {
        if let Some(values) = manifest.get(kind) {
            targets.extend(values.as_array()?);
        }
    }
    let mut paths = Vec::new();
    for target in targets {
        let table = target.as_table()?;
        if let Some(path) = table.get("path") {
            paths.push(PathBuf::from(path.as_str()?));
        }
    }
    Some(paths)
}

fn is_automatic_target(path: &Path) -> bool {
    if matches!(
        path.to_str(),
        Some("src/lib.rs" | "src/main.rs" | "build.rs")
    ) {
        return true;
    }
    for base in ["src/bin", "examples", "tests", "benches"] {
        if let Ok(relative) = path.strip_prefix(base) {
            let count = relative.components().count();
            if count == 1
                || (count == 2 && relative.file_name().is_some_and(|name| name == "main.rs"))
            {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::module_dir;

    #[test]
    fn a_module_file_declares_its_children_beside_its_own_name() {
        assert_eq!(
            module_dir(Path::new("crates/x/src/backends.rs")),
            Some(PathBuf::from("crates/x/src/backends"))
        );
    }

    #[test]
    fn a_mod_file_declares_its_children_in_its_own_directory() {
        assert_eq!(
            module_dir(Path::new("crates/x/src/backends/mod.rs")),
            Some(PathBuf::from("crates/x/src/backends"))
        );
    }
}

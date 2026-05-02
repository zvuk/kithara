//! File-system traversal helpers shared across xtask namespaces.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::Result;
use glob::Pattern;

use super::scope::Scope;

/// Recursively collect all `.rs` files under `dir`, returning them sorted.
pub(crate) fn walk_rs_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    walk_rs_files_inner(dir, &mut out)?;
    out.sort();
    Ok(out)
}

fn walk_rs_files_inner(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Ok(());
    };
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            walk_rs_files_inner(&path, out)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
    Ok(())
}

/// Compile glob patterns; ignore unparseable ones (rare; a config error would surface elsewhere).
pub(crate) fn compile_globs(raw: &[String]) -> Vec<Pattern> {
    raw.iter().filter_map(|s| Pattern::new(s).ok()).collect()
}

pub(crate) fn matches_any(patterns: &[Pattern], rel: &Path) -> bool {
    let s = rel.to_string_lossy();
    patterns.iter().any(|p| p.matches(&s))
}

/// Walk `.rs` files under each root in `scope` (defaults to `<workspace>/crates`
/// when scope is empty). Results are de-duplicated and sorted.
pub(crate) fn workspace_rs_files_scoped(
    workspace_root: &Path,
    scope: &Scope,
) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for root in scope.roots(workspace_root) {
        walk_rs_files_inner(&root, &mut out)?;
    }
    out.sort();
    out.dedup();
    Ok(out)
}

pub(crate) fn relative_to<'a>(root: &Path, full: &'a Path) -> &'a Path {
    full.strip_prefix(root).unwrap_or(full)
}

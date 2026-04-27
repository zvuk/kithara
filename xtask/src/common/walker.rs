//! File-system traversal helpers shared across xtask namespaces.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::Result;
use glob::Pattern;

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

/// Walk all `.rs` files under `<workspace_root>/crates`, returning paths relative to workspace root.
pub(crate) fn workspace_rs_files(workspace_root: &Path) -> Result<Vec<PathBuf>> {
    let crates_dir = workspace_root.join("crates");
    walk_rs_files(&crates_dir)
}

pub(crate) fn relative_to<'a>(root: &Path, full: &'a Path) -> &'a Path {
    full.strip_prefix(root).unwrap_or(full)
}

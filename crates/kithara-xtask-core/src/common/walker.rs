use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result};
use glob::Pattern;

use super::{project::ProjectConfig, scope::Scope};

/// Recursively collect all `.rs` files under `dir`, returning them sorted.
///
/// # Errors
///
/// Returns an error if a directory entry cannot be read while walking `dir`.
pub fn walk_rs_files(dir: &Path) -> Result<Vec<PathBuf>> {
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
#[must_use]
pub fn compile_globs(raw: &[String]) -> Vec<Pattern> {
    raw.iter().filter_map(|s| Pattern::new(s).ok()).collect()
}

#[must_use]
pub fn matches_any(patterns: &[Pattern], rel: &Path) -> bool {
    let s = rel.to_string_lossy();
    patterns.iter().any(|p| p.matches(&s))
}

/// Walk `.rs` files under each root in `scope` (defaults to `<workspace>/crates`
/// when scope is empty). Results are de-duplicated and sorted.
///
/// # Errors
///
/// Returns an error if a directory entry cannot be read while walking any
/// scoped root.
pub fn workspace_rs_files_scoped(workspace_root: &Path, scope: &Scope) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let excludes = WorkspaceScanExcludes::from_root(workspace_root)?;
    for root in scope.roots(workspace_root) {
        walk_rs_files_inner(&root, &mut out)?;
    }
    out.retain(|path| !excludes.matches(path));
    out.sort();
    out.dedup();
    Ok(out)
}

#[must_use]
pub fn relative_to<'a>(root: &Path, full: &'a Path) -> &'a Path {
    full.strip_prefix(root).unwrap_or(full)
}

struct WorkspaceScanExcludes {
    workspace_root: PathBuf,
    patterns: Vec<Pattern>,
}

impl WorkspaceScanExcludes {
    fn from_root(workspace_root: &Path) -> Result<Self> {
        let config = ProjectConfig::load(workspace_root).with_context(|| {
            format!(
                "loading workspace scan config from {}",
                workspace_root.display()
            )
        })?;
        Ok(Self {
            workspace_root: workspace_root.to_path_buf(),
            patterns: compile_globs(&config.workspace_scan.exclude),
        })
    }

    fn matches(&self, path: &Path) -> bool {
        if self.patterns.is_empty() {
            return false;
        }
        let rel = relative_to(&self.workspace_root, path);
        matches_any(&self.patterns, rel)
    }
}

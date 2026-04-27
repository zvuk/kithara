//! Per-file metric helpers used by multiple checks.

use std::path::{Path, PathBuf};

use anyhow::Result;
use glob::Pattern;

use crate::util::walk_rs_files;

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

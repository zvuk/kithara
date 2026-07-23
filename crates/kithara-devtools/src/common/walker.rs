use std::{
    fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context as _, Result, bail};
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

/// Walk tracked text files under the workspace or requested scope.
///
/// Empty scope walks every tracked workspace file. Non-empty scope keeps files
/// whose workspace-relative path is covered by the same `Scope` matching used
/// for ratcheted lint keys. Git is the owner of "tracked" and ignored path
/// state here, so ignored/untracked build outputs never enter the scan.
///
/// # Errors
///
/// Returns an error if `git ls-files` fails or emits a non-UTF-8 path.
pub fn workspace_text_files_scoped(workspace_root: &Path, scope: &Scope) -> Result<Vec<PathBuf>> {
    let excludes = WorkspaceScanExcludes::from_root(workspace_root)?;
    let mut out = git_tracked_files(workspace_root)?;
    out.retain(|path| {
        is_lint_text_file(path)
            && !is_symlink(path)
            && !excludes.matches(path)
            && scoped_path_match(workspace_root, scope, path)
    });
    out.sort();
    out.dedup();
    Ok(out)
}

pub(crate) fn workspace_tracked_files(workspace_root: &Path) -> Result<Vec<PathBuf>> {
    git_tracked_files(workspace_root)
}

#[must_use]
pub fn relative_to<'a>(root: &Path, full: &'a Path) -> &'a Path {
    full.strip_prefix(root).unwrap_or(full)
}

fn git_tracked_files(workspace_root: &Path) -> Result<Vec<PathBuf>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(workspace_root)
        .arg("ls-files")
        .arg("-z")
        .output()
        .with_context(|| format!("list tracked files under {}", workspace_root.display()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "git ls-files failed under {}: {stderr}",
            workspace_root.display()
        );
    }
    let stdout = String::from_utf8(output.stdout).with_context(|| {
        format!(
            "git ls-files emitted a non-UTF-8 path under {}",
            workspace_root.display()
        )
    })?;
    Ok(stdout
        .split('\0')
        .filter(|rel| !rel.is_empty())
        .map(|rel| workspace_root.join(rel))
        .collect())
}

fn scoped_path_match(workspace_root: &Path, scope: &Scope, path: &Path) -> bool {
    if scope.is_empty() {
        return true;
    }
    let rel = relative_to(workspace_root, path)
        .to_string_lossy()
        .replace('\\', "/");
    scope.key_in_scope(&rel)
}

// Symlinked docs are pointers; their content is scanned at the real path,
// and relative links only resolve there.
fn is_symlink(path: &Path) -> bool {
    path.symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink())
}

fn is_lint_text_file(path: &Path) -> bool {
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if matches!(
        file_name,
        "Dockerfile" | "dockerfile" | "justfile" | "Justfile"
    ) || file_name.ends_with(".Dockerfile")
    {
        return true;
    }
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            matches!(
                ext.to_ascii_lowercase().as_str(),
                "rs" | "md"
                    | "toml"
                    | "yml"
                    | "yaml"
                    | "mdc"
                    | "dockerfile"
                    | "sh"
                    | "bash"
                    | "zsh"
                    | "fish"
            )
        })
        || has_shell_shebang(path)
}

fn has_shell_shebang(path: &Path) -> bool {
    if path.extension().is_some() {
        return false;
    }
    let Ok(file) = fs::File::open(path) else {
        return false;
    };
    let mut first_line = String::new();
    let mut reader = BufReader::new(file);
    if reader.read_line(&mut first_line).is_err() {
        return false;
    }
    let line = first_line.trim_end();
    line.starts_with("#!")
        && (line.contains("/sh")
            || line.contains("/bash")
            || line.contains("/zsh")
            || line.contains("/fish")
            || line.contains("env sh")
            || line.contains("env bash")
            || line.contains("env zsh")
            || line.contains("env fish"))
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

use std::{
    fs,
    fs::File,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context as _, Result, bail};
use glob::{MatchOptions, Pattern};

use super::{exclude::module_files, project::ProjectConfig, scope::Scope};

/// Recursively collect all `.rs` files under `dir`, returning them sorted.
///
/// # Errors
///
/// Returns an error if a directory entry cannot be read while walking `dir`.
pub fn walk_rs_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    collect_rs_files(root, &mut out)?;
    out.sort();
    Ok(out)
}

fn is_rs_file(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()) == Some("rs")
}

/// A root is either a directory to walk or a single `.rs` file. Scope roots
/// come from `--path`, which accepts a file, so a root that is not a directory
/// must still be collected rather than silently yielding nothing.
fn collect_rs_files(root: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    if is_rs_file(root) && root.is_file() {
        out.push(root.to_path_buf());
        return Ok(());
    }
    walk_rs_files_inner(root, out)
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
        } else if is_rs_file(&path) {
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

/// Match with `*` confined to a single path segment.
///
/// `matches_any` lets `*` cross `/`, so a pattern like `crates/*/README.md`
/// there also selects a README nested any number of directories deeper. Use
/// this when a pattern's depth is part of what it selects; `**` still spans
/// segments.
#[must_use]
pub fn matches_any_segmented(patterns: &[Pattern], rel: &Path) -> bool {
    const OPTIONS: MatchOptions = MatchOptions {
        case_sensitive: true,
        require_literal_separator: true,
        require_literal_leading_dot: false,
    };
    let s = rel.to_string_lossy();
    patterns.iter().any(|p| p.matches_with(&s, OPTIONS))
}

/// Walk scoped Rust sources using Cargo targets or the project's default roots.
/// Explicit scopes override defaults; results are de-duplicated and sorted.
///
/// # Errors
///
/// Returns an error if a directory entry cannot be read while walking any
/// scoped root.
pub fn workspace_rs_files_scoped(workspace_root: &Path, scope: &Scope) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let excludes = WorkspaceScanExcludes::from_root(workspace_root)?;
    let targets = scope.root_targets();
    for root in scope.roots_with_defaults(workspace_root, &excludes.default_roots) {
        if targets.contains(&root) {
            out.extend(module_files(&root, |path| excludes.matches(path))?);
        } else {
            collect_rs_files(&root, &mut out)?;
        }
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

/// Symlinked docs are pointers; their content is scanned at the real path,
/// and relative links only resolve there.
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
    let Ok(file) = File::open(path) else {
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
    default_roots: Vec<PathBuf>,
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
            default_roots: config.workspace_scan.default_roots,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_root_yields_that_file() {
        let root = tempfile::tempdir().expect("tempdir");
        let file = root.path().join("lib.rs");
        fs::write(&file, "fn main() {}").expect("write");

        let found = walk_rs_files(&file).expect("walk");

        assert_eq!(found, vec![file]);
    }

    #[test]
    fn a_non_rust_file_root_yields_nothing() {
        let root = tempfile::tempdir().expect("tempdir");
        let file = root.path().join("notes.md");
        fs::write(&file, "text").expect("write");

        let found = walk_rs_files(&file).expect("walk");

        assert!(found.is_empty(), "expected no files, got {found:?}");
    }

    #[test]
    fn a_directory_root_yields_its_rust_files() {
        let root = tempfile::tempdir().expect("tempdir");
        fs::create_dir(root.path().join("nested")).expect("mkdir");
        let top = root.path().join("lib.rs");
        let nested = root.path().join("nested").join("mod.rs");
        fs::write(&top, "").expect("write");
        fs::write(&nested, "").expect("write");
        fs::write(root.path().join("README.md"), "").expect("write");

        let found = walk_rs_files(root.path()).expect("walk");

        assert_eq!(found, vec![top, nested]);
    }
}

//! File-system / package scope filter for lint subcommands.
//!
//! Used by all xtask lint namespaces (`arch`, `style`, `idioms`) to
//! restrict which files / packages a check inspects. When empty, scope
//! matches everything under `<workspace>/crates/` (legacy default).
//!
//! When non-empty, the scope is built from clap flags `--crate <name>`
//! (repeatable) and `--path <workspace-relative-path>` (repeatable). Paths
//! can point anywhere under the workspace (`tests/`, `xtask/`,
//! `examples/`, …), not only under `crates/`.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use cargo_metadata::{Metadata, Package};
use clap::ValueEnum;

/// Downstream tool whose flag dialect `Scope::flags_for` emits. Used by
/// `cargo xtask scope --for=<TOOL>` for shell-substitution in justfile
/// recipes (`just audit`, etc.).
#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum Tool {
    Xtask,
    Clippy,
    Fmt,
    #[clap(name = "ast-grep")]
    AstGrep,
}

const TOP_LEVEL_DIRS: &[&str] = &["tests", "xtask", "examples", "benches"];

#[derive(Debug)]
enum ScopeToken {
    Crate(String),
    CratePath { path: PathBuf },
    NonCratePath(PathBuf),
}

fn classify_token(tok: &str, workspace_root: &Path) -> Result<ScopeToken> {
    let norm = tok.strip_prefix("./").unwrap_or(tok);
    let path = Path::new(norm);
    let mut comps = path.components();
    let first = comps
        .next()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .unwrap_or_default();

    if first == "crates" {
        return Ok(ScopeToken::CratePath {
            path: PathBuf::from(norm),
        });
    }
    if TOP_LEVEL_DIRS.contains(&first.as_str()) {
        return Ok(ScopeToken::NonCratePath(PathBuf::from(norm)));
    }
    if workspace_root.join("crates").join(norm).is_dir() {
        return Ok(ScopeToken::Crate(norm.to_string()));
    }
    bail!("audit: unknown scope '{tok}' (not a crate name nor a known top-level path)")
}

#[derive(Debug, Default, Clone)]
pub(crate) struct Scope {
    /// Crate names from `--crate <name>`. Resolved against
    /// `<workspace>/crates/<name>/`.
    pub(crate) crates: Vec<String>,
    /// Workspace-relative paths from `--path <p>`.
    pub(crate) paths: Vec<PathBuf>,
    /// True when any path is outside `crates/` (e.g. `tests/`, `xtask/`).
    /// Tells `flags_for(Clippy|Fmt)` to fall back to workspace-wide because
    /// those tools don't accept arbitrary path scoping.
    pub(crate) has_noncrate_path: bool,
}

impl Scope {
    pub(crate) fn new(crates: Vec<String>, paths: Vec<PathBuf>) -> Self {
        Self {
            crates,
            paths,
            has_noncrate_path: false,
        }
    }

    /// Resolve a list of user-supplied scope tokens (typically from
    /// positional `cargo xtask scope` args) into a `Scope`. Each token is
    /// classified as a bare crate name, a `crates/<name>[/...]` path, or
    /// a non-crate workspace path (`tests/`, `xtask/`, …).
    pub(crate) fn resolve(tokens: &[String], workspace_root: &Path) -> Result<Self> {
        let mut scope = Self::default();
        for tok in tokens {
            match classify_token(tok, workspace_root)? {
                ScopeToken::Crate(name) => scope.crates.push(name),
                ScopeToken::CratePath { path } => scope.paths.push(path),
                ScopeToken::NonCratePath(p) => {
                    scope.paths.push(p);
                    scope.has_noncrate_path = true;
                }
            }
        }
        Ok(scope)
    }

    /// Crate names extracted from `crates/<name>[/...]` paths. Used by
    /// clippy/fmt flag emission to convert path scope to `-p NAME`.
    fn crate_names_from_paths(&self) -> Vec<String> {
        self.paths
            .iter()
            .filter_map(|p| {
                let mut c = p.components();
                let first = c.next()?.as_os_str().to_string_lossy().into_owned();
                if first != "crates" {
                    return None;
                }
                let second = c.next()?.as_os_str().to_string_lossy().into_owned();
                Some(second)
            })
            .collect()
    }

    /// Emit space-separated flag tokens for the requested downstream tool.
    /// See `cargo xtask scope` documentation for the full output table.
    pub(crate) fn flags_for(&self, tool: Tool) -> Vec<String> {
        let is_empty = self.crates.is_empty() && self.paths.is_empty();
        let crate_paths_names = self.crate_names_from_paths();
        match tool {
            Tool::Xtask => {
                if is_empty {
                    return vec![];
                }
                let mut out = Vec::new();
                for c in &self.crates {
                    out.push("--crate".into());
                    out.push(c.clone());
                }
                for p in &self.paths {
                    out.push("--path".into());
                    out.push(p.display().to_string());
                }
                out
            }
            Tool::Clippy => {
                if is_empty || self.has_noncrate_path {
                    return vec!["--workspace".into()];
                }
                let mut out = Vec::new();
                for c in self.crates.iter().chain(crate_paths_names.iter()) {
                    out.push("-p".into());
                    out.push(c.clone());
                }
                if out.is_empty() {
                    return vec!["--workspace".into()];
                }
                // `--no-deps` keeps clippy focused on the target crate(s).
                // Without it, dead-code in a dependency (e.g. items behind
                // a non-default feature like kithara-hls's `internal`)
                // surfaces as errors during a per-crate audit. Workspace
                // clippy stays in `just lint-fast`.
                out.push("--no-deps".into());
                out
            }
            Tool::Fmt => {
                if is_empty || self.has_noncrate_path {
                    return vec!["--all".into()];
                }
                let mut out = Vec::new();
                for c in self.crates.iter().chain(crate_paths_names.iter()) {
                    out.push("-p".into());
                    out.push(c.clone());
                }
                if out.is_empty() {
                    out.push("--all".into());
                }
                out
            }
            Tool::AstGrep => {
                if is_empty {
                    return vec![];
                }
                let mut out = Vec::new();
                for c in &self.crates {
                    out.push(format!("crates/{c}"));
                }
                for p in &self.paths {
                    out.push(p.display().to_string());
                }
                out
            }
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.crates.is_empty() && self.paths.is_empty()
    }

    /// Test whether a workspace-relative path/key is in scope. Used to
    /// filter baseline entries (which are recorded as workspace-relative
    /// path keys) before ratchet diffing on a scoped run — without this,
    /// a scoped run would report out-of-scope baseline entries as
    /// "improvements" because they don't appear in the scoped report.
    pub(crate) fn key_in_scope(&self, key: &str) -> bool {
        if self.is_empty() {
            return true;
        }
        // Some baseline keys are not paths (e.g., `direction` uses
        // `from_crate -> to_crate`). For non-path keys, fall back to
        // matching by crate name appearing in the key.
        self.crates
            .iter()
            .any(|c| key.starts_with(&format!("crates/{c}/")) || key.contains(c.as_str()))
            || self.paths.iter().any(|p| {
                let prefix = p
                    .strip_prefix("./")
                    .unwrap_or(p)
                    .to_string_lossy()
                    .replace('\\', "/");
                key.starts_with(&format!("{prefix}/")) || key == prefix
            })
    }

    /// Roots to walk. Empty scope → `<root>/crates` (the legacy default,
    /// preserves current xtask behavior). Non-empty → one entry per crate
    /// or path, resolved against `workspace_root`.
    pub(crate) fn roots(&self, workspace_root: &Path) -> Vec<PathBuf> {
        if self.is_empty() {
            return vec![workspace_root.join("crates")];
        }
        let mut out = Vec::new();
        for c in &self.crates {
            out.push(workspace_root.join("crates").join(c));
        }
        for p in &self.paths {
            let normalized = p.strip_prefix("./").unwrap_or(p);
            let abs = if normalized.is_absolute() {
                normalized.to_path_buf()
            } else {
                workspace_root.join(normalized)
            };
            out.push(abs);
        }
        out
    }
}

/// Filter cargo-metadata workspace packages by scope.
///
/// Empty scope returns all workspace members. Non-empty scope keeps only
/// packages whose names appear in `scope.crates`. `scope.paths` is
/// ignored here (path scoping applies to walker-based checks only).
pub(crate) fn packages_in_scope<'a>(metadata: &'a Metadata, scope: &Scope) -> Vec<&'a Package> {
    if scope.is_empty() {
        return metadata.workspace_packages().into_iter().collect();
    }
    if scope.crates.is_empty() {
        return Vec::new();
    }
    metadata
        .workspace_packages()
        .into_iter()
        .filter(|p| scope.crates.iter().any(|c| c == p.name.as_str()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_scope_roots_default_to_crates_dir() {
        let s = Scope::default();
        let roots = s.roots(Path::new("/workspace"));
        assert_eq!(roots, vec![PathBuf::from("/workspace/crates")]);
    }

    #[test]
    fn scoped_roots_resolve_against_workspace() {
        let s = Scope::new(
            vec!["kithara-abr".into()],
            vec!["tests/tests".into(), "./xtask/src".into()],
        );
        let roots = s.roots(Path::new("/workspace"));
        assert_eq!(
            roots,
            vec![
                PathBuf::from("/workspace/crates/kithara-abr"),
                PathBuf::from("/workspace/tests/tests"),
                PathBuf::from("/workspace/xtask/src"),
            ]
        );
    }

    #[test]
    fn empty_scope_emits_workspace_for_clippy_and_fmt() {
        let s = Scope::default();
        assert_eq!(s.flags_for(Tool::Clippy), vec!["--workspace"]);
        assert_eq!(s.flags_for(Tool::Fmt), vec!["--all"]);
        assert_eq!(s.flags_for(Tool::Xtask), Vec::<String>::new());
        assert_eq!(s.flags_for(Tool::AstGrep), Vec::<String>::new());
    }

    #[test]
    fn crate_scope_flags() {
        let s = Scope::new(vec!["kithara-queue".into()], vec![]);
        assert_eq!(
            s.flags_for(Tool::Clippy),
            vec!["-p", "kithara-queue", "--no-deps"]
        );
        assert_eq!(s.flags_for(Tool::Fmt), vec!["-p", "kithara-queue"]);
        assert_eq!(s.flags_for(Tool::Xtask), vec!["--crate", "kithara-queue"]);
        assert_eq!(s.flags_for(Tool::AstGrep), vec!["crates/kithara-queue"]);
    }

    #[test]
    fn crate_path_extracts_crate_name_for_clippy() {
        let s = Scope::new(vec![], vec!["crates/kithara-abr/src".into()]);
        assert_eq!(
            s.flags_for(Tool::Clippy),
            vec!["-p", "kithara-abr", "--no-deps"]
        );
        assert_eq!(
            s.flags_for(Tool::Xtask),
            vec!["--path", "crates/kithara-abr/src"]
        );
    }

    #[test]
    fn noncrate_path_falls_back_for_clippy_fmt() {
        let mut s = Scope::default();
        s.paths.push("tests/tests".into());
        s.has_noncrate_path = true;
        assert_eq!(s.flags_for(Tool::Clippy), vec!["--workspace"]);
        assert_eq!(s.flags_for(Tool::Fmt), vec!["--all"]);
        assert_eq!(s.flags_for(Tool::Xtask), vec!["--path", "tests/tests"]);
        assert_eq!(s.flags_for(Tool::AstGrep), vec!["tests/tests"]);
    }

    #[test]
    fn classify_unknown_scope_errors() {
        let err = classify_token("foobar_nonsense", Path::new("/nonexistent"));
        assert!(err.is_err());
    }

    #[test]
    fn classify_top_level_dirs_are_noncrate_paths() {
        let res = classify_token("tests/tests", Path::new("/nonexistent"));
        assert!(matches!(res, Ok(ScopeToken::NonCratePath(_))));
    }

    #[test]
    fn classify_crates_path_extracts_path() {
        let res = classify_token("crates/kithara-abr/src", Path::new("/nonexistent"));
        assert!(matches!(res, Ok(ScopeToken::CratePath { .. })));
    }

    #[test]
    fn classify_strips_leading_dot_slash() {
        let res = classify_token("./xtask/src", Path::new("/nonexistent"));
        assert!(matches!(res, Ok(ScopeToken::NonCratePath(_))));
    }
}

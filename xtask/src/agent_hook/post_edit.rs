use std::{
    env,
    ffi::{OsStr, OsString},
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};

use super::input::HookInput;

const PROJECT_MARKER: &str = "xtask/agent-hook";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FormatTarget {
    Rust,
    Toml,
    Json,
}

#[derive(Debug, Eq, PartialEq)]
struct FormatCommand {
    program: &'static str,
    args: Vec<OsString>,
}

impl FormatTarget {
    fn name(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::Toml => "toml",
            Self::Json => "json",
        }
    }
}

pub(super) fn run(input: &HookInput) -> Result<()> {
    if !matches!(input.tool_name.as_str(), "Write" | "Edit" | "MultiEdit") {
        return Ok(());
    }
    let Some(path) = input.tool_input.file_path.as_deref() else {
        return Ok(());
    };
    let root = hook_project_root(input)?;
    let Some(path) = resolve_edited_path(&root, Path::new(path))? else {
        return Ok(());
    };
    let Some(target) = format_target_for_path(&path) else {
        return Ok(());
    };
    let command = format_command(target, &root, &path);
    let status = Command::new(command.program)
        .current_dir(root)
        .args(command.args)
        .status()
        .with_context(|| format!("run post-edit formatter for {}", target.name()))?;
    if !status.success() {
        bail!(
            "post-edit formatter for {} failed (exit code {:?})",
            target.name(),
            status.code()
        );
    }
    Ok(())
}

fn format_command(target: FormatTarget, root: &Path, path: &Path) -> FormatCommand {
    match target {
        FormatTarget::Rust => FormatCommand {
            program: "rustup",
            args: vec![
                "run".into(),
                "nightly".into(),
                "rustfmt".into(),
                "--edition".into(),
                "2024".into(),
                "--config-path".into(),
                root.join("rustfmt.toml").into_os_string(),
                "--config".into(),
                "skip_children=true".into(),
                path.as_os_str().to_owned(),
            ],
        },
        FormatTarget::Toml => FormatCommand {
            program: "taplo",
            args: vec!["format".into(), path.as_os_str().to_owned()],
        },
        FormatTarget::Json => FormatCommand {
            program: "tidy-json",
            args: vec![
                "--indent".into(),
                "2".into(),
                "--write".into(),
                path.as_os_str().to_owned(),
            ],
        },
    }
}

fn hook_project_root(input: &HookInput) -> Result<PathBuf> {
    if let Some(root) = env::var_os("KITHARA_AGENT_HOOK_ROOT").filter(|value| !value.is_empty()) {
        let root = fs::canonicalize(PathBuf::from(root))
            .context("resolve launcher-owned Kithara project root")?;
        if !root.join(PROJECT_MARKER).is_file() {
            bail!("launcher-owned project root is not a Kithara checkout");
        }
        return Ok(root);
    }

    let current_dir = env::current_dir().context("resolve current hook directory")?;
    let project_dir = env::var_os("CLAUDE_PROJECT_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            if input.cwd.is_empty() {
                None
            } else {
                Some(PathBuf::from(&input.cwd))
            }
        })
        .unwrap_or_else(|| current_dir.clone());
    let project_dir = if project_dir.is_absolute() {
        project_dir
    } else {
        current_dir.join(project_dir)
    };
    let project_dir = fs::canonicalize(&project_dir)
        .with_context(|| format!("resolve hook project directory {}", project_dir.display()))?;
    find_project_root(&project_dir)
        .with_context(|| format!("find Kithara project root from {}", project_dir.display()))
}

fn find_project_root(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|path| path.join(PROJECT_MARKER).is_file())
        .map(Path::to_path_buf)
}

fn resolve_edited_path(root: &Path, path: &Path) -> Result<Option<PathBuf>> {
    let root = fs::canonicalize(root)
        .with_context(|| format!("resolve project root {}", root.display()))?;
    let candidate = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    let candidate = fs::canonicalize(&candidate)
        .with_context(|| format!("resolve edited path {}", candidate.display()))?;
    Ok(candidate.starts_with(&root).then_some(candidate))
}

fn format_target_for_path(path: &Path) -> Option<FormatTarget> {
    if path.file_name() == Some(OsStr::new("Cargo.toml")) {
        return None;
    }
    match path.extension().and_then(OsStr::to_str) {
        Some("rs") => Some(FormatTarget::Rust),
        Some("toml") => Some(FormatTarget::Toml),
        Some("json" | "jsonc") => Some(FormatTarget::Json),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use anyhow::Result;

    use super::{FormatTarget, format_command, format_target_for_path, resolve_edited_path};

    #[test]
    fn rejects_post_edit_path_outside_project_root() -> Result<()> {
        let root = tempfile::tempdir()?;
        let outside = tempfile::tempdir()?;
        let outside_file = outside.path().join("outside.rs");
        fs::write(&outside_file, "fn outside() {}")?;

        assert!(resolve_edited_path(root.path(), &outside_file)?.is_none());
        Ok(())
    }

    #[test]
    fn uses_path_scoped_rust_formatter() {
        let root = Path::new("/repo");
        let path = Path::new("/repo/crates/foo/src/lib.rs");
        let command = format_command(FormatTarget::Rust, root, path);

        assert_eq!(command.program, "rustup");
        assert_eq!(command.args.last(), Some(&path.as_os_str().to_owned()));
        assert!(command.args.iter().any(|arg| arg == "skip_children=true"));
    }

    #[test]
    fn uses_path_scoped_data_formatters() {
        let root = Path::new("/repo");
        let toml = Path::new("/repo/.config/foo.toml");
        let json = Path::new("/repo/tests/foo.jsonc");

        let toml_command = format_command(FormatTarget::Toml, root, toml);
        assert_eq!(toml_command.program, "taplo");
        assert_eq!(toml_command.args.last(), Some(&toml.as_os_str().to_owned()));

        let json_command = format_command(FormatTarget::Json, root, json);
        assert_eq!(json_command.program, "tidy-json");
        assert_eq!(json_command.args.last(), Some(&json.as_os_str().to_owned()));
    }

    #[test]
    fn maps_post_edit_format_targets() {
        assert_eq!(
            format_target_for_path(Path::new("crates/foo/src/lib.rs")),
            Some(FormatTarget::Rust)
        );
        assert_eq!(
            format_target_for_path(Path::new("crates/foo/Cargo.toml")),
            None
        );
        assert_eq!(
            format_target_for_path(Path::new(".config/foo.toml")),
            Some(FormatTarget::Toml)
        );
        assert_eq!(
            format_target_for_path(Path::new("tests/foo.jsonc")),
            Some(FormatTarget::Json)
        );
        assert_eq!(format_target_for_path(Path::new("README.md")), None);
    }
}

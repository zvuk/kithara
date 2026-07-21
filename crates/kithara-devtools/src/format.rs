use std::{
    ffi::{OsStr, OsString},
    fs,
    path::{Component, Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};
use clap::{Args, ValueEnum};

use crate::{
    Ctx, manifest,
    manifest::{DependencyOrderArgs, ManifestArgs, ManifestCommand},
    util::{check_tool, ensure_clean_tree},
};

const CHUNK_SIZE: usize = 128;

#[derive(Debug, Args)]
pub struct FormatArgs {
    /// Restrict the formatter to one or more targets.
    #[arg(long = "only", value_enum)]
    only: Vec<FormatTarget>,
    /// Skip the dirty-tree gate when formatting in place.
    #[arg(long = "allow-dirty")]
    allow_dirty: bool,
    /// Check formatting without modifying files.
    #[arg(long)]
    check: bool,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, ValueEnum)]
enum FormatTarget {
    Rust,
    Manifest,
    Toml,
    Json,
    Markdown,
}

#[derive(Clone, Copy)]
enum FileKind {
    Toml,
    Json,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PathFormatTarget {
    Rust,
    Toml,
    Json,
}

#[derive(Debug, Eq, PartialEq)]
struct PathFormatCommand {
    program: &'static str,
    args: Vec<OsString>,
}

pub(crate) fn run(args: &FormatArgs, ctx: &Ctx) -> Result<()> {
    if !args.check {
        ensure_clean_tree(args.allow_dirty, "format")?;
    }

    for target in selected_targets(args) {
        run_target(target, args.check, ctx)
            .with_context(|| format!("format target `{}`", target.name()))?;
    }
    Ok(())
}

/// Formats one edited Rust, TOML, or JSON path below `root`.
///
/// # Errors
/// Returns an error if path resolution or formatting fails.
pub fn format_path(root: &Path, path: &Path) -> Result<()> {
    let root = fs::canonicalize(root)
        .with_context(|| format!("resolve formatting root {}", root.display()))?;
    let candidate = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    let path = fs::canonicalize(&candidate)
        .with_context(|| format!("resolve formatting path {}", candidate.display()))?;
    if !path.starts_with(&root) {
        bail!(
            "formatting path {} is outside formatting root {}",
            path.display(),
            root.display()
        );
    }
    let Some(target) = path_format_target(&path) else {
        return Ok(());
    };
    let command = path_format_command(target, &root, &path);
    let status = Command::new(command.program)
        .current_dir(&root)
        .args(&command.args)
        .status()
        .with_context(|| format!("run path formatter for {}", target.name()))?;
    if !status.success() {
        bail!(
            "path formatter for {} failed (exit code {:?})",
            target.name(),
            status.code()
        );
    }
    Ok(())
}

impl PathFormatTarget {
    fn name(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::Toml => "toml",
            Self::Json => "json",
        }
    }
}

fn path_format_target(path: &Path) -> Option<PathFormatTarget> {
    if path.file_name() == Some(OsStr::new("Cargo.toml")) {
        return None;
    }
    match path.extension().and_then(OsStr::to_str) {
        Some("rs") => Some(PathFormatTarget::Rust),
        Some("toml") => Some(PathFormatTarget::Toml),
        Some("json" | "jsonc") => Some(PathFormatTarget::Json),
        _ => None,
    }
}

fn path_format_command(target: PathFormatTarget, root: &Path, path: &Path) -> PathFormatCommand {
    match target {
        PathFormatTarget::Rust => PathFormatCommand {
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
        PathFormatTarget::Toml => PathFormatCommand {
            program: "taplo",
            args: vec!["format".into(), path.as_os_str().to_owned()],
        },
        PathFormatTarget::Json => PathFormatCommand {
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

fn selected_targets(args: &FormatArgs) -> Vec<FormatTarget> {
    if args.only.is_empty() {
        vec![
            FormatTarget::Rust,
            FormatTarget::Manifest,
            FormatTarget::Toml,
            FormatTarget::Json,
        ]
    } else {
        let mut targets = args.only.clone();
        targets.sort_unstable();
        targets.dedup();
        targets
    }
}

fn run_target(target: FormatTarget, check: bool, ctx: &Ctx) -> Result<()> {
    match target {
        FormatTarget::Rust => run_rustfmt(check),
        FormatTarget::Manifest => run_manifest_format(check, ctx),
        FormatTarget::Toml => run_toml_format(check),
        FormatTarget::Json => run_json_format(check),
        FormatTarget::Markdown => run_markdown_format(check),
    }
}

impl FormatTarget {
    fn name(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::Manifest => "manifest",
            Self::Toml => "toml",
            Self::Json => "json",
            Self::Markdown => "markdown",
        }
    }
}

fn run_rustfmt(check: bool) -> Result<()> {
    let mut args = vec!["+nightly", "fmt", "--all"];
    if check {
        args.push("--check");
    }
    run_status("cargo", &args)
}

fn run_manifest_format(check: bool, ctx: &Ctx) -> Result<()> {
    if !check {
        check_tool(
            "cargo",
            &["sort", "--version"],
            "cargo install --locked cargo-sort",
        )?;
        run_status(
            "cargo",
            &[
                "sort",
                "--workspace",
                "--grouped",
                "--config",
                ".config/tomlfmt.toml",
            ],
        )?;
    }

    manifest::run(
        &ManifestArgs {
            command: ManifestCommand::DependencyOrder(DependencyOrderArgs {
                fix: !check,
                allow_dirty: true,
            }),
        },
        ctx,
    )
}

fn run_toml_format(check: bool) -> Result<()> {
    check_tool("taplo", &["--version"], "cargo install --locked taplo-cli")?;
    let files = collect_files(Path::new("."), FileKind::Toml)?;
    let mut args = vec!["format"];
    if check {
        args.push("--check");
    }
    run_path_status("taplo", &args, &files)
}

fn run_json_format(check: bool) -> Result<()> {
    check_tool(
        "tidy-json",
        &["--version"],
        "cargo install --locked tidy-json",
    )?;
    let files = collect_files(Path::new("."), FileKind::Json)?;
    let mut args = vec!["--indent", "2"];
    if check {
        args.push("--check");
    } else {
        args.push("--write");
    }
    run_path_status("tidy-json", &args, &files)
}

fn run_markdown_format(check: bool) -> Result<()> {
    check_tool(
        "mdfmt",
        &["--version"],
        "cargo install --locked md-formatter",
    )?;

    let mut args = vec![
        "AGENTS.md",
        "README.md",
        "CONTEXT.md",
        "CONTRIBUTING.md",
        "CHANGELOG.md",
        "TESTING.md",
        "SECURITY.md",
        "ARCHITECTURE.md",
        "CODE_OF_CONDUCT.md",
        "docs/guides",
        "docs/workflows",
        "docs/agents",
        "docs/skills",
        "crates",
        "tests",
        "apple",
        "android",
        "xtask",
        "--width",
        "100",
        "--wrap",
        "preserve",
        "--ordered-list",
        "one",
        "--exclude",
        "docs/plans",
    ];
    if check {
        args.push("--check");
    } else {
        args.push("--write");
    }
    run_status("mdfmt", &args)
}

fn run_status(program: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(program)
        .args(args)
        .status()
        .with_context(|| format!("failed to run `{}`", command_line(program, args)))?;
    if !status.success() {
        bail!(
            "`{}` failed (exit code {:?})",
            command_line(program, args),
            status.code()
        );
    }
    Ok(())
}

fn run_path_status(program: &str, args: &[&str], files: &[PathBuf]) -> Result<()> {
    if files.is_empty() {
        return Ok(());
    }

    for chunk in files.chunks(CHUNK_SIZE) {
        let status = Command::new(program)
            .args(args)
            .args(chunk)
            .status()
            .with_context(|| format!("failed to run `{program}`"))?;
        if !status.success() {
            bail!(
                "`{}` failed on {} file(s) (exit code {:?})",
                command_line(program, args),
                chunk.len(),
                status.code()
            );
        }
    }
    Ok(())
}

fn command_line(program: &str, args: &[&str]) -> String {
    if args.is_empty() {
        program.to_owned()
    } else {
        format!("{program} {}", args.join(" "))
    }
}

fn collect_files(root: &Path, kind: FileKind) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    visit_dir(root, root, kind, &mut files)?;
    files.sort();
    Ok(files)
}

fn visit_dir(root: &Path, dir: &Path, kind: FileKind, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("read directory {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        let rel = path.strip_prefix(root).unwrap_or(&path);
        let file_type = entry.file_type()?;

        if file_type.is_dir() {
            if should_skip_dir(rel) {
                continue;
            }
            visit_dir(root, &path, kind, files)?;
        } else if file_type.is_file() && matches_file_kind(rel, kind) {
            files.push(path);
        }
    }
    Ok(())
}

fn matches_file_kind(path: &Path, kind: FileKind) -> bool {
    match kind {
        FileKind::Toml => {
            path.extension() == Some(OsStr::new("toml"))
                && path.file_name() != Some(OsStr::new("Cargo.toml"))
        }
        FileKind::Json => {
            matches!(
                path.extension().and_then(OsStr::to_str),
                Some("json" | "jsonc")
            ) && path.file_name() != Some(OsStr::new("package-lock.json"))
        }
    }
}

fn should_skip_dir(path: &Path) -> bool {
    let components = path
        .components()
        .filter_map(component_os_str)
        .collect::<Vec<_>>();
    let Some(first) = components.first() else {
        return false;
    };

    if *first == OsStr::new(".build")
        || *first == OsStr::new(".git")
        || *first == OsStr::new("docs-build")
        || *first == OsStr::new("target")
    {
        return true;
    }
    if first.to_string_lossy().starts_with("target-flash") {
        return true;
    }
    is_apple_build_dir(&components)
        || is_android_build_dir(&components)
        || path_starts_with(&components, &["tests", "fuzz", "artifacts"])
        || path_starts_with(&components, &["tests", "fuzz", "corpus"])
        || path_starts_with(&components, &["tests", "fuzz", "coverage"])
}

fn component_os_str(component: Component<'_>) -> Option<&OsStr> {
    match component {
        Component::Normal(value) => Some(value),
        _ => None,
    }
}

fn path_starts_with(components: &[&OsStr], prefix: &[&str]) -> bool {
    components.len() >= prefix.len()
        && components
            .iter()
            .zip(prefix)
            .all(|(actual, expected)| *actual == OsStr::new(expected))
}

fn is_android_build_dir(components: &[&OsStr]) -> bool {
    components.len() >= 3
        && components[0] == OsStr::new("android")
        && components[2] == OsStr::new("build")
}

fn is_apple_build_dir(components: &[&OsStr]) -> bool {
    components.first() == Some(&OsStr::new("apple"))
        && components.iter().any(|component| {
            *component == OsStr::new(".build")
                || *component == OsStr::new("build")
                || *component == OsStr::new("DerivedData")
        })
}

#[cfg(test)]
mod tests {
    use std::{ffi::OsString, fs, path::Path};

    use anyhow::Result;

    use super::{
        FileKind, PathFormatTarget, format_path, matches_file_kind, path_format_command,
        should_skip_dir,
    };

    #[test]
    fn path_formatter_skips_cargo_manifest() -> Result<()> {
        let root = tempfile::tempdir()?;
        let manifest = root.path().join("Cargo.toml");
        fs::write(&manifest, "[package]\nname='example'\n")?;

        format_path(root.path(), &manifest)?;

        assert_eq!(fs::read_to_string(manifest)?, "[package]\nname='example'\n");
        Ok(())
    }

    #[test]
    fn path_formatter_rejects_paths_outside_root() -> Result<()> {
        let root = tempfile::tempdir()?;
        let outside = tempfile::tempdir()?;
        let path = outside.path().join("outside.rs");
        fs::write(&path, "fn outside() {}\n")?;

        let error = format_path(root.path(), &path).expect_err("outside path must fail");

        assert!(format!("{error:#}").contains("outside formatting root"));
        Ok(())
    }

    #[test]
    fn path_formatter_commands_match_owned_tool_semantics() {
        let root = Path::new("/repo");
        let rust = Path::new("/repo/crates/example/src/lib.rs");
        let toml = Path::new("/repo/.config/example.toml");
        let json = Path::new("/repo/tests/example.jsonc");

        let rust_command = path_format_command(PathFormatTarget::Rust, root, rust);
        assert_eq!(rust_command.program, "rustup");
        assert_eq!(rust_command.args.first(), Some(&OsString::from("run")));
        assert!(rust_command.args.iter().any(|arg| arg == "nightly"));
        assert!(rust_command.args.iter().any(|arg| arg == "--edition"));
        assert!(rust_command.args.iter().any(|arg| arg == "2024"));
        assert!(
            rust_command
                .args
                .iter()
                .any(|arg| arg == "skip_children=true")
        );
        assert_eq!(rust_command.args.last(), Some(&rust.as_os_str().to_owned()));

        let toml_command = path_format_command(PathFormatTarget::Toml, root, toml);
        assert_eq!(toml_command.program, "taplo");
        assert_eq!(toml_command.args, [OsString::from("format"), toml.into()]);

        let json_command = path_format_command(PathFormatTarget::Json, root, json);
        assert_eq!(json_command.program, "tidy-json");
        assert_eq!(json_command.args.last(), Some(&json.as_os_str().to_owned()));
        assert!(json_command.args.iter().any(|arg| arg == "--write"));
    }

    #[test]
    fn file_kind_filters_cargo_and_package_lock() {
        assert!(matches_file_kind(
            Path::new(".config/xtask.toml"),
            FileKind::Toml
        ));
        assert!(!matches_file_kind(Path::new("Cargo.toml"), FileKind::Toml));
        assert!(matches_file_kind(
            Path::new("tests/webdriver.json"),
            FileKind::Json
        ));
        assert!(matches_file_kind(
            Path::new("config/app.jsonc"),
            FileKind::Json
        ));
        assert!(!matches_file_kind(
            Path::new("package-lock.json"),
            FileKind::Json
        ));
    }

    #[test]
    fn directory_filter_matches_formatter_skip_policy() {
        assert!(should_skip_dir(Path::new("target")));
        assert!(should_skip_dir(Path::new("target-flash-native")));
        assert!(should_skip_dir(Path::new(".build")));
        assert!(should_skip_dir(Path::new("docs-build")));
        assert!(should_skip_dir(Path::new("apple/.build")));
        assert!(should_skip_dir(Path::new("apple/build")));
        assert!(should_skip_dir(Path::new("apple/Examples/Demo/build")));
        assert!(should_skip_dir(Path::new(
            "apple/Examples/Demo/build/DerivedData"
        )));
        assert!(should_skip_dir(Path::new("android/app/build")));
        assert!(should_skip_dir(Path::new("tests/fuzz/artifacts")));
        assert!(!should_skip_dir(Path::new(".config")));
        assert!(!should_skip_dir(Path::new("crates/kithara/src")));
    }
}

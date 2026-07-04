use std::{
    ffi::OsStr,
    fs,
    path::{Component, Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};
use clap::{Args, ValueEnum};
use kithara_xtask_core::util::{check_tool, ensure_clean_tree};

use crate::{
    manifest,
    manifest::{DependencyOrderArgs, ManifestArgs, ManifestCommand},
};

const CHUNK_SIZE: usize = 128;

#[derive(Debug, Args)]
pub(crate) struct FormatArgs {
    /// Check formatting without modifying files.
    #[arg(long)]
    check: bool,
    /// Skip the dirty-tree gate when formatting in place.
    #[arg(long = "allow-dirty")]
    allow_dirty: bool,
    /// Restrict the formatter to one or more targets.
    #[arg(long = "only", value_enum)]
    only: Vec<FormatTarget>,
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

pub(crate) fn run(args: &FormatArgs) -> Result<()> {
    if !args.check {
        ensure_clean_tree(args.allow_dirty, "format")?;
    }

    for target in selected_targets(args) {
        run_target(target, args.check)
            .with_context(|| format!("format target `{}`", target.name()))?;
    }
    Ok(())
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

fn run_target(target: FormatTarget, check: bool) -> Result<()> {
    match target {
        FormatTarget::Rust => run_rustfmt(check),
        FormatTarget::Manifest => run_manifest_format(check),
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

fn run_manifest_format(check: bool) -> Result<()> {
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

    manifest::run(&ManifestArgs {
        command: ManifestCommand::DependencyOrder(DependencyOrderArgs {
            fix: !check,
            allow_dirty: true,
        }),
    })
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
        ".docs/guides",
        ".docs/workflows",
        ".docs/agents",
        ".docs/skills",
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
        ".docs/plans",
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
    use std::path::Path;

    use super::{FileKind, matches_file_kind, should_skip_dir};

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

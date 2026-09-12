use std::{
    ffi::{OsStr, OsString},
    fs::{self, File},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context, Result, bail};
use clap::{Args, ValueEnum};

use crate::{
    Ctx,
    common::tools::ToolsConfig,
    manifest,
    manifest::{DependencyOrderArgs, ManifestArgs, ManifestCommand},
    util::{check_tool, ensure_clean_tree},
    verdict::ChildFailure,
};

const CHUNK_SIZE: usize = 128;

const GIT_LISTING_ARGS: [&str; 4] = ["ls-files", "--cached", "--others", "--exclude-standard"];

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
struct PathFormatCommand<'a> {
    program: &'a str,
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
pub fn format_path(root: &Path, path: &Path, tools: &ToolsConfig) -> Result<()> {
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
    let command = path_format_command(target, &root, &path, tools);
    if target == PathFormatTarget::Json {
        return write_json_format(&command, &root, &path);
    }
    let status = Command::new(command.program)
        .current_dir(&root)
        .args(&command.args)
        .status()
        .with_context(|| format!("run path formatter for {}", target.name()))?;
    if !status.success() {
        return Err(ChildFailure::inherited(
            format!("path formatter for {}", target.name()),
            status.code(),
        ));
    }
    Ok(())
}

impl PathFormatTarget {
    const fn name(self) -> &'static str {
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

fn path_format_command<'a>(
    target: PathFormatTarget,
    root: &Path,
    path: &Path,
    tools: &'a ToolsConfig,
) -> PathFormatCommand<'a> {
    match target {
        PathFormatTarget::Rust => PathFormatCommand {
            program: "rustup",
            args: vec![
                "run".into(),
                nightly_toolchain().into(),
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
            program: tools.program("taplo"),
            args: vec!["format".into(), path.as_os_str().to_owned()],
        },
        PathFormatTarget::Json => json_format_command(tools),
    }
}

fn json_format_command(tools: &ToolsConfig) -> PathFormatCommand<'_> {
    const ARGS: [&str; 4] = ["--indent", "2", "--stdin", "--stdout"];

    PathFormatCommand {
        program: tools.program("tidy-json"),
        args: ARGS.iter().map(OsString::from).collect(),
    }
}

fn write_json_format(command: &PathFormatCommand<'_>, root: &Path, path: &Path) -> Result<()> {
    if is_commented_json(path.strip_prefix(root).unwrap_or(path)) {
        return Ok(());
    }
    let Some(formatted) = json_reformatted(command, path)? else {
        return Ok(());
    };
    fs::write(path, formatted).with_context(|| format!("write {}", path.display()))
}

/// The tidy-json rendering of `path`, or `None` when the file already carries
/// it byte for byte.
fn json_reformatted(command: &PathFormatCommand<'_>, path: &Path) -> Result<Option<Vec<u8>>> {
    let current = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let input = File::open(path).with_context(|| format!("open {} for stdin", path.display()))?;
    let output = Command::new(command.program)
        .args(&command.args)
        .stdin(Stdio::from(input))
        .output()
        .with_context(|| format!("run `{}` on {}", command.program, path.display()))?;
    if !output.status.success() || !output.stderr.is_empty() {
        return Err(ChildFailure::captured(
            format!("`{}` on {}", command.program, path.display()),
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ));
    }
    if output.stdout.is_empty() {
        bail!(
            "`{}` printed nothing for {}",
            command.program,
            path.display()
        );
    }
    Ok((output.stdout != current).then_some(output.stdout))
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
        FormatTarget::Toml => run_toml_format(check, ctx),
        FormatTarget::Json => run_json_format(check, ctx),
        FormatTarget::Markdown => run_markdown_format(check, ctx),
    }
}

impl FormatTarget {
    const fn name(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::Manifest => "manifest",
            Self::Toml => "toml",
            Self::Json => "json",
            Self::Markdown => "markdown",
        }
    }
}

/// The nightly channel the repository pins. CI exports it from
/// `.config/ci-pins.toml`; a plain `nightly` is the local-development default.
fn nightly_toolchain() -> String {
    std::env::var("KITHARA_NIGHTLY_TOOLCHAIN")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "nightly".to_owned())
}

fn run_rustfmt(check: bool) -> Result<()> {
    let toolchain = format!("+{}", nightly_toolchain());
    let mut args = vec![toolchain.as_str(), "fmt", "--all"];
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
            ctx.config
                .tools
                .install_hint("cargo-sort", "cargo install --locked cargo-sort"),
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

fn run_toml_format(check: bool, ctx: &Ctx) -> Result<()> {
    let program = ctx.config.tools.program("taplo");
    check_tool(
        program,
        &["--version"],
        ctx.config
            .tools
            .install_hint("taplo", "cargo install --locked taplo-cli"),
    )?;
    let files = collect_files(Path::new("."), FileKind::Toml)?;
    let mut args = vec!["format"];
    if check {
        args.push("--check");
    }
    run_path_status(program, &args, &files)
}

fn run_json_format(check: bool, ctx: &Ctx) -> Result<()> {
    let command = json_format_command(&ctx.config.tools);
    check_tool(
        command.program,
        &["--version"],
        ctx.config
            .tools
            .install_hint("tidy-json", "cargo install --locked tidy-json"),
    )?;
    let mut unformatted = Vec::new();
    for file in collect_files(Path::new("."), FileKind::Json)? {
        let Some(formatted) = json_reformatted(&command, &file)? else {
            continue;
        };
        if check {
            unformatted.push(file);
        } else {
            fs::write(&file, formatted).with_context(|| format!("write {}", file.display()))?;
        }
    }
    if !unformatted.is_empty() {
        let listed = unformatted
            .iter()
            .map(|path| format!("  {}", path.display()))
            .collect::<Vec<_>>()
            .join("\n");
        bail!(
            "{} JSON file(s) are not formatted:\n{listed}",
            unformatted.len()
        );
    }
    Ok(())
}

fn run_markdown_format(check: bool, ctx: &Ctx) -> Result<()> {
    let program = ctx.config.tools.program("mdfmt");
    check_tool(
        program,
        &["--version"],
        ctx.config
            .tools
            .install_hint("mdfmt", "cargo install --locked md-formatter"),
    )?;

    let mut args = vec![
        "AGENTS.md",
        "README.md",
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
    run_status(program, &args)
}

fn run_status(program: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(program)
        .args(args)
        .status()
        .with_context(|| format!("failed to run `{}`", command_line(program, args)))?;
    if !status.success() {
        return Err(ChildFailure::inherited(
            format!("`{}`", command_line(program, args)),
            status.code(),
        ));
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
            return Err(ChildFailure::inherited(
                format!(
                    "`{}` on {} file(s)",
                    command_line(program, args),
                    chunk.len()
                ),
                status.code(),
            ));
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
    Ok(select_files(&git_listing(root)?, kind))
}

/// Every path git accounts for below `root`: tracked files, plus new ones that
/// `.gitignore` does not cover.
fn git_listing(root: &Path) -> Result<Vec<PathBuf>> {
    let output = Command::new("git")
        .current_dir(root)
        .args(GIT_LISTING_ARGS)
        .output()
        .with_context(|| {
            format!(
                "run `git {}` in {}",
                GIT_LISTING_ARGS.join(" "),
                root.display()
            )
        })?;
    if !output.status.success() {
        return Err(ChildFailure::captured(
            format!("`git {}` in {}", GIT_LISTING_ARGS.join(" "), root.display()),
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(PathBuf::from)
        .collect())
}

fn select_files(listing: &[PathBuf], kind: FileKind) -> Vec<PathBuf> {
    let mut files = listing
        .iter()
        .filter(|path| !path.parent().is_some_and(should_skip_dir) && matches_file_kind(path, kind))
        .cloned()
        .collect::<Vec<_>>();
    files.sort();
    files
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
                && !is_commented_json(path)
        }
    }
}

/// tidy-json prints plain JSON; these three carry comments.
fn is_commented_json(path: &Path) -> bool {
    const COMMENTED: [&str; 3] = [".zed/debug.json", ".zed/settings.json", ".zed/tasks.json"];

    COMMENTED
        .iter()
        .any(|commented| path == Path::new(commented))
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
    if first.to_string_lossy().starts_with("target-flash")
        || *first == OsStr::new("target-audit-clippy")
    {
        return true;
    }
    is_apple_build_dir(&components)
        || is_android_build_dir(&components)
        || path_starts_with(&components, &["tests", "fuzz", "artifacts"])
        || path_starts_with(&components, &["tests", "fuzz", "corpus"])
        || path_starts_with(&components, &["tests", "fuzz", "coverage"])
}

const fn component_os_str(component: Component<'_>) -> Option<&OsStr> {
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
    use std::{
        ffi::OsString,
        fs,
        path::{Path, PathBuf},
        process::Command,
    };

    use anyhow::Result;

    use super::{
        FileKind, PathFormatTarget, collect_files, format_path, matches_file_kind,
        nightly_toolchain, path_format_command, select_files, should_skip_dir,
    };
    use crate::common::tools::ToolsConfig;

    #[test]
    fn a_configured_formatter_reaches_the_path_command() {
        let tools: ToolsConfig = toml::from_str(
            r#"
            [taplo]
            program = "/opt/pinned/bin/taplo"
            "#,
        )
        .expect("the tools table parses");

        let command = path_format_command(
            PathFormatTarget::Toml,
            Path::new("/root"),
            Path::new("/root/a.toml"),
            &tools,
        );

        assert_eq!(command.program, "/opt/pinned/bin/taplo");
    }

    #[test]
    fn path_formatter_skips_cargo_manifest() -> Result<()> {
        let root = tempfile::tempdir()?;
        let manifest = root.path().join("Cargo.toml");
        fs::write(&manifest, "[package]\nname='example'\n")?;

        format_path(root.path(), &manifest, &ToolsConfig::default())?;

        assert_eq!(fs::read_to_string(manifest)?, "[package]\nname='example'\n");
        Ok(())
    }

    #[test]
    fn path_formatter_rejects_paths_outside_root() -> Result<()> {
        let root = tempfile::tempdir()?;
        let outside = tempfile::tempdir()?;
        let path = outside.path().join("outside.rs");
        fs::write(&path, "fn outside() {}\n")?;

        let error = format_path(root.path(), &path, &ToolsConfig::default())
            .expect_err("outside path must fail");

        assert!(format!("{error:#}").contains("outside formatting root"));
        Ok(())
    }

    #[test]
    fn path_formatter_commands_match_owned_tool_semantics() {
        let root = Path::new("/repo");
        let rust = Path::new("/repo/crates/example/src/lib.rs");
        let toml = Path::new("/repo/.config/example.toml");
        let tools = ToolsConfig::default();

        let rust_command = path_format_command(PathFormatTarget::Rust, root, rust, &tools);
        assert_eq!(rust_command.program, "rustup");
        assert_eq!(rust_command.args.first(), Some(&OsString::from("run")));
        assert!(
            rust_command
                .args
                .iter()
                .any(|arg| arg == nightly_toolchain().as_str()),
            "rustfmt must run under the toolchain the repository pins"
        );
        assert!(rust_command.args.iter().any(|arg| arg == "--edition"));
        assert!(rust_command.args.iter().any(|arg| arg == "2024"));
        assert!(
            rust_command
                .args
                .iter()
                .any(|arg| arg == "skip_children=true")
        );
        assert_eq!(rust_command.args.last(), Some(&rust.as_os_str().to_owned()));

        let toml_command = path_format_command(PathFormatTarget::Toml, root, toml, &tools);
        assert_eq!(toml_command.program, "taplo");
        assert_eq!(toml_command.args, [OsString::from("format"), toml.into()]);
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
        assert!(!matches_file_kind(
            Path::new(".zed/settings.json"),
            FileKind::Json
        ));
        assert!(matches_file_kind(
            Path::new(".zed/later.json"),
            FileKind::Json
        ));
    }

    #[test]
    fn directory_filter_matches_formatter_skip_policy() {
        assert!(should_skip_dir(Path::new("target")));
        assert!(should_skip_dir(Path::new("target-audit-clippy")));
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

    #[test]
    fn json_path_command_formats_through_stdin() {
        let json = Path::new("/repo/tests/example.jsonc");
        let tools = ToolsConfig::default();

        let command = path_format_command(PathFormatTarget::Json, Path::new("/repo"), json, &tools);

        assert_eq!(command.program, "tidy-json");
        assert_eq!(
            command.args,
            ["--indent", "2", "--stdin", "--stdout"].map(OsString::from)
        );
        assert!(
            !command
                .args
                .iter()
                .any(|arg| arg == json.as_os_str() || arg == "--write"),
            "tidy-json reads the file from stdin"
        );
    }

    #[test]
    fn path_formatter_leaves_commented_zed_files_alone() -> Result<()> {
        let root = tempfile::tempdir()?;
        fs::create_dir(root.path().join(".zed"))?;
        let path = root.path().join(".zed/settings.json");
        let source = "{\n  // keep this\n  \"a\": 1\n}\n";
        fs::write(&path, source)?;

        format_path(root.path(), &path, &ToolsConfig::default())?;

        assert_eq!(fs::read_to_string(&path)?, source);
        Ok(())
    }

    #[test]
    fn json_targets_keep_dot_directory_files() {
        let listing = [
            PathBuf::from(".claude/settings.json"),
            PathBuf::from(".zed/settings.json"),
            PathBuf::from("crates/kithara-ui/assets/lottie/probe.json"),
        ];

        let targets = select_files(&listing, FileKind::Json);

        assert_eq!(
            targets,
            [
                PathBuf::from(".claude/settings.json"),
                PathBuf::from("crates/kithara-ui/assets/lottie/probe.json")
            ]
        );
    }

    #[test]
    fn json_targets_come_from_the_git_listing() -> Result<()> {
        let root = tempfile::tempdir()?;
        let root = root.path();
        fs::write(root.join(".gitignore"), ".claude/worktrees/\n")?;
        fs::create_dir_all(root.join(".claude/worktrees/checkout"))?;
        fs::create_dir(root.join(".zed"))?;
        for (path, body) in [
            (".claude/settings.json", "{}\n"),
            (".zed/settings.json", "{}\n"),
            (".claude/worktrees/checkout/foo.json", "{}\n"),
            ("app.json", "{}\n"),
        ] {
            fs::write(root.join(path), body)?;
        }
        git(root, &["init", "-q"])?;
        git(
            root,
            &["add", ".claude/settings.json", ".zed/settings.json"],
        )?;

        let files = collect_files(root, FileKind::Json)?;

        assert_eq!(
            files,
            [
                PathBuf::from(".claude/settings.json"),
                PathBuf::from("app.json")
            ]
        );
        Ok(())
    }

    fn git(root: &Path, args: &[&str]) -> Result<()> {
        let status = Command::new("git").current_dir(root).args(args).status()?;
        assert!(status.success(), "git {args:?} must succeed");
        Ok(())
    }
}

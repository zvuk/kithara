use std::process::Command;

use anyhow::{Result, bail};
use cargo_metadata::MetadataCommand;
use clap::{Args, ValueEnum};

use crate::util::check_tool;

struct Consts;
impl Consts {
    const INSTALL_HINT: &'static str = "cargo install similarity-rs";

    const EXCLUDED_CRATES: &'static [&'static str] = &[
        "kithara-workspace-hack",
        "kithara-test-utils",
        "kithara-test-macros",
        "kithara-ffi-macros",
    ];
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum Profile {
    /// Blocking, low-noise: 0.96 / min-lines 12 / skip-test / fail-on-duplicates.
    Audit,
    /// Informational, default for `just similarity`: 0.85 / 10 / skip-test.
    Advisory,
    /// Strict comprehensive: 0.80 / 8 / includes tests. Used by `just health`.
    Strict,
}

#[derive(Debug, Args)]
pub(crate) struct SimilarityArgs {
    #[arg(long, value_enum, default_value_t = Profile::Advisory)]
    pub profile: Profile,
    /// Optional roots to scan. Empty = all production crate `src/` dirs
    /// (excluding test-utils and proc-macro crates).
    pub paths: Vec<String>,
}

pub(crate) fn run(args: &SimilarityArgs) -> Result<()> {
    check_tool("similarity-rs", &["--version"], Consts::INSTALL_HINT)?;
    let mut cmd = Command::new("similarity-rs");

    let (threshold, min_lines, skip_test, fail_on_dup) = match args.profile {
        Profile::Audit => ("0.96", "12", true, true),
        Profile::Advisory => ("0.85", "10", true, false),
        Profile::Strict => ("0.80", "8", false, false),
    };

    cmd.arg("--threshold").arg(threshold);
    cmd.arg("--min-lines").arg(min_lines);
    if skip_test {
        cmd.arg("--skip-test");
    }
    if fail_on_dup {
        cmd.arg("--fail-on-duplicates");
    }
    cmd.arg("--exclude").arg("target");
    cmd.arg("--exclude").arg(".claude");
    cmd.arg("--exclude").arg(".worktrees");

    let roots = if args.paths.is_empty() {
        default_roots()?
    } else {
        args.paths
            .iter()
            .filter(|p| !path_is_in_excluded_crate(p))
            .cloned()
            .collect::<Vec<_>>()
    };
    if roots.is_empty() {
        return Ok(());
    }
    for r in &roots {
        cmd.arg(r);
    }

    let status = cmd.status()?;
    if !status.success() {
        bail!("similarity-rs failed (exit code {:?})", status.code());
    }
    Ok(())
}

fn path_is_in_excluded_crate(path: &str) -> bool {
    Consts::EXCLUDED_CRATES.iter().any(|crate_name| {
        let prefix = format!("crates/{crate_name}/");
        path == format!("crates/{crate_name}") || path.starts_with(&prefix)
    })
}

fn default_roots() -> Result<Vec<String>> {
    let metadata = MetadataCommand::new().no_deps().exec()?;
    let workspace_root = metadata.workspace_root.as_std_path();
    let mut out = Vec::new();
    for pkg in metadata.workspace_packages() {
        let name = pkg.name.as_str();
        if Consts::EXCLUDED_CRATES.contains(&name) {
            continue;
        }
        let src = workspace_root.join("crates").join(name).join("src");
        if src.is_dir() {
            out.push(format!("crates/{name}/src"));
        }
    }
    out.sort();
    Ok(out)
}

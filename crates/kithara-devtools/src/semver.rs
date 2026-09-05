use std::{collections::BTreeSet, process::Command};

use anyhow::{Context, Result, bail};
use clap::Args;
use serde::Deserialize;

use crate::{Ctx, util::check_tool, verdict::NotClean};

struct Consts;
impl Consts {
    const INSTALL_HINT: &'static str = "cargo install cargo-semver-checks";
}

#[derive(Debug, Args)]
pub struct SemverArgs {
    /// Revision to compare against. The workspace is unpublished, so the
    /// baseline is a revision here rather than a crates.io release.
    #[arg(long, default_value = "HEAD~1")]
    pub baseline: String,
}

/// Runs the configured public-surface comparison against a revision.
///
/// The wrapper exists for the lease, not for the flags. `cargo semver-checks`
/// builds rustdoc for every crate twice and clones the baseline revision into
/// `CARGO_TARGET_DIR`, which on Linux runners is a volume the host budgets. A
/// bare `cargo` invocation holds no job lease, and between two crates it holds
/// no `.cargo-lock` either, so a reclaim in a sibling job reads the whole
/// target as abandoned: the clone this run had been reading for seven minutes
/// disappeared, and the failure surfaced as `failed to canonicalize manifest
/// path` on whichever crate came next. Running under `xtask` puts the lane
/// behind the same lease every other build lane holds.
pub(crate) fn run(args: &SemverArgs, ctx: &Ctx) -> Result<()> {
    check_tool(
        "cargo",
        &["semver-checks", "--version"],
        ctx.config
            .tools
            .install_hint("cargo-semver-checks", Consts::INSTALL_HINT),
    )?;
    let baseline_members = members_at(&args.baseline)?;
    let (packages, missing) =
        packages_to_compare(&ctx.config.health.semver_packages, &baseline_members);
    for name in missing {
        println!(
            "semver-checks: {name} has no counterpart in {}, nothing to compare",
            args.baseline
        );
    }
    if packages.is_empty() {
        return Ok(());
    }
    let status = Command::new("cargo")
        .args(semver_args(&args.baseline, &packages))
        .status()?;
    if !status.success() {
        return Err(NotClean::reported("semver-checks"));
    }
    Ok(())
}

fn packages_to_compare<'a>(
    configured: &'a [String],
    baseline_members: &BTreeSet<String>,
) -> (Vec<&'a str>, Vec<&'a str>) {
    configured
        .iter()
        .map(String::as_str)
        .partition(|name| baseline_members.contains(*name))
}

fn semver_args<'a>(baseline: &'a str, packages: &[&'a str]) -> Vec<&'a str> {
    let mut args = vec!["semver-checks", "check-release"];
    for package in packages {
        args.extend(["--package", *package]);
    }
    args.extend(["--baseline-rev", baseline, "--release-type", "minor"]);
    args
}

/// Workspace member names recorded in `baseline`'s lockfile.
fn members_at(baseline: &str) -> Result<BTreeSet<String>> {
    let path = format!("{baseline}:Cargo.lock");
    let output = Command::new("git").args(["show", &path]).output()?;
    if !output.status.success() {
        bail!(
            "git show {path} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let lock = String::from_utf8(output.stdout).context("baseline Cargo.lock is not UTF-8")?;
    local_packages(&lock)
}

/// The path-sourced entries of a lockfile.
///
/// Registry and git packages carry a `source` there; path packages do not,
/// which is what separates the workspace's own crates from its dependencies.
fn local_packages(lock: &str) -> Result<BTreeSet<String>> {
    let lockfile: Lockfile = toml::from_str(lock).context("parse baseline Cargo.lock")?;
    Ok(lockfile
        .package
        .into_iter()
        .filter(|package| package.source.is_none())
        .map(|package| package.name)
        .collect())
}

#[derive(Debug, Deserialize)]
struct Lockfile {
    #[serde(default)]
    package: Vec<LockPackage>,
}

#[derive(Debug, Deserialize)]
struct LockPackage {
    source: Option<String>,
    name: String,
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{local_packages, packages_to_compare, semver_args};

    const LOCK: &str = r#"
version = 4

[[package]]
name = "anyhow"
version = "1.0.100"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "kithara-decode"
version = "0.0.1-alpha4"
dependencies = ["anyhow"]

[[package]]
name = "firewheel-web-audio"
version = "0.7.0"
source = "git+https://github.com/example/firewheel#0000000"
"#;

    #[test]
    fn local_packages_keeps_path_entries() -> anyhow::Result<()> {
        let names = local_packages(LOCK)?;

        assert!(names.contains("kithara-decode"));
        Ok(())
    }

    #[test]
    fn local_packages_drops_registry_entries() -> anyhow::Result<()> {
        let names = local_packages(LOCK)?;

        assert!(!names.contains("anyhow"));
        Ok(())
    }

    #[test]
    fn local_packages_drops_git_entries() -> anyhow::Result<()> {
        let names = local_packages(LOCK)?;

        assert!(!names.contains("firewheel-web-audio"));
        Ok(())
    }

    #[test]
    fn configured_semver_scope_excludes_packages_without_a_baseline() {
        let configured = vec!["kithara".to_owned(), "kithara-new".to_owned()];
        let baseline = BTreeSet::from(["kithara".to_owned()]);

        let (packages, missing) = packages_to_compare(&configured, &baseline);

        assert_eq!(packages, ["kithara"]);
        assert_eq!(missing, ["kithara-new"]);
    }

    #[test]
    fn semver_command_compares_only_named_packages_as_a_minor_release() {
        let args = semver_args("origin/main", &["kithara", "kithara-ffi"]);

        assert!(args.windows(2).any(|pair| pair == ["--package", "kithara"]));
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--package", "kithara-ffi"])
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--release-type", "minor"])
        );
        assert!(!args.contains(&"--workspace"));
    }
}

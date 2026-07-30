use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{self, Write as _},
    path::{Component, Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};
use cargo_metadata::Metadata;
use clap::{Args, ValueEnum};
use serde::{Deserialize, Serialize};

use crate::{
    Ctx,
    common::walker::{relative_to, walk_rs_files},
    util::check_tool,
};

mod analysis;
mod behavior;
mod catalog;
mod report;
mod shape;

struct Consts;
impl Consts {
    const CONFIG_REL: &'static str = ".config/similarity.toml";

    const INSTALL_HINT: &'static str = "cargo install similarity-rs";
}

/// Crate exclusions for similarity scans, loaded from
/// `.config/similarity.toml`. Project-agnostic: when the file is absent
/// no crates are excluded - every project supplies its own list.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct SimilarityConfig {
    excluded_crates: Vec<String>,
    types: TypeConfig,
    #[serde(skip)]
    active_dependencies: BTreeSet<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct TypeConfig {
    families: BTreeMap<String, TypeFamilyConfig>,
    relations: Vec<TypeRelationConfig>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TypeFamilyConfig {
    default_similarity: f64,
    members: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct TypeRelationConfig {
    left: String,
    right: String,
    similarity: f64,
    substitution: Substitution,
    direction: Direction,
    caveats: Vec<String>,
}

impl Default for TypeRelationConfig {
    fn default() -> Self {
        Self {
            left: String::new(),
            right: String::new(),
            similarity: 0.0,
            substitution: Substitution::Conditional,
            direction: Direction::Bidirectional,
            caveats: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum Substitution {
    Safe,
    #[default]
    Conditional,
    Incompatible,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum Direction {
    #[default]
    Bidirectional,
    LeftToRight,
    RightToLeft,
}

impl SimilarityConfig {
    pub(crate) fn load(workspace_root: &Path) -> Result<Self> {
        let path = workspace_root.join(Consts::CONFIG_REL);
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = fs::read_to_string(&path)
            .with_context(|| format!("read similarity config: {}", path.display()))?;
        toml::from_str(&text)
            .with_context(|| format!("parse similarity config: {}", path.display()))
    }
}

fn activate_dependencies(config: &mut SimilarityConfig, metadata: &Metadata) {
    config.active_dependencies = metadata
        .workspace_packages()
        .iter()
        .flat_map(|package| package.dependencies.iter())
        .map(|dependency| dependency.name.to_string())
        .collect();
}

#[derive(Debug, Clone, Copy, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum Profile {
    /// Blocking, low-noise: 0.96 / min-lines 12 / skip-test / fail-on-duplicates.
    Audit,
    /// Informational, default for `just lint similarity`: 0.85 / 10 / skip-test.
    Advisory,
    /// Strict comprehensive: 0.80 / 8 / includes tests. Used by
    /// `just ci health`.
    Strict,
}

impl Profile {
    fn as_str(self) -> &'static str {
        match self {
            Self::Audit => "audit",
            Self::Advisory => "advisory",
            Self::Strict => "strict",
        }
    }
}

#[derive(Debug, Args)]
pub struct SimilarityArgs {
    #[arg(long, value_enum, default_value_t = Profile::Advisory)]
    pub profile: Profile,
    /// Ignore project-default crate exclusions for a complete or explicit scan.
    #[arg(long)]
    pub include_default_excluded: bool,
    /// Optional roots to scan. Empty = all production crate `src/` dirs
    /// (excluding test-utils and proc-macro crates).
    pub paths: Vec<String>,
}

pub(crate) fn run(args: &SimilarityArgs, ctx: &Ctx) -> Result<()> {
    let (threshold, min_lines, skip_test, fail_on_dup) = match args.profile {
        Profile::Audit => ("0.96", "12", true, true),
        Profile::Advisory => ("0.85", "10", true, false),
        Profile::Strict => ("0.80", "8", false, false),
    };

    let metadata = ctx.metadata()?;
    let mut config = ctx.similarity.clone();
    activate_dependencies(&mut config, metadata);
    let no_exclusions = Vec::new();
    let excluded = if args.include_default_excluded {
        &no_exclusions
    } else {
        &config.excluded_crates
    };
    let include_tests = matches!(args.profile, Profile::Strict);

    let roots = if args.paths.is_empty() {
        default_roots(metadata, excluded, include_tests)
    } else {
        args.paths
            .iter()
            .filter(|p| !path_is_in_excluded_crate(p, excluded))
            .cloned()
            .collect::<Vec<_>>()
    };
    if roots.is_empty() {
        return Ok(());
    }
    let files = source_files(&ctx.root, &roots, include_tests)?;
    let native = analysis::analyze_files(&files, &config, include_tests)?;
    let revision = revision(&ctx.root);
    let output = ctx.root.join("target/similarity").join(&revision);
    let artifacts = report::write(
        &output,
        &revision,
        args.profile,
        &roots,
        args.include_default_excluded,
        &native,
    )?;
    writeln!(io::stdout().lock(), "==> {}", artifacts.document.display())?;

    check_tool("similarity-rs", &["--version"], Consts::INSTALL_HINT)?;
    let mut cmd = Command::new("similarity-rs");
    cmd.current_dir(&ctx.root);
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
    for r in &roots {
        cmd.arg(r);
    }

    let status = cmd.status()?;
    if !status.success() {
        bail!("similarity-rs failed (exit code {:?})", status.code());
    }
    Ok(())
}

fn source_files(
    workspace_root: &Path,
    roots: &[String],
    include_tests: bool,
) -> Result<Vec<(String, PathBuf)>> {
    let mut files = BTreeMap::new();
    for root in roots {
        let path = workspace_root.join(root);
        if !path.exists() {
            bail!("similarity scan root does not exist: {}", path.display());
        }
        let candidates = if path.is_file() {
            vec![path]
        } else {
            walk_rs_files(&path)?
        };
        for candidate in candidates {
            if candidate
                .extension()
                .and_then(|extension| extension.to_str())
                != Some("rs")
                || candidate.components().any(|component| {
                    matches!(
                        component,
                        Component::Normal(name) if name == "target" || name == ".git"
                    )
                })
                || !include_tests && is_test_path(&candidate)
            {
                continue;
            }
            let relative = relative_to(workspace_root, &candidate)
                .to_string_lossy()
                .replace('\\', "/");
            files.insert(relative, candidate);
        }
    }
    Ok(files.into_iter().collect())
}

fn is_test_path(path: &Path) -> bool {
    path.components().any(|component| {
        matches!(component, Component::Normal(name) if name == "tests" || name == "benches")
    }) || path.file_name().is_some_and(|name| {
        matches!(
            name.to_str(),
            Some("test.rs" | "tests.rs" | "bench.rs" | "benches.rs")
        )
    })
}

fn revision(root: &Path) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--short=12", "HEAD"])
        .output();
    output
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|revision| revision.trim().to_string())
        .filter(|revision| !revision.is_empty())
        .unwrap_or_else(|| "working-tree".to_string())
}

fn path_is_in_excluded_crate(path: &str, excluded: &[String]) -> bool {
    excluded.iter().any(|crate_name| {
        let prefix = format!("crates/{crate_name}/");
        path == format!("crates/{crate_name}") || path.starts_with(&prefix)
    })
}

fn default_roots(metadata: &Metadata, excluded: &[String], include_tests: bool) -> Vec<String> {
    let workspace_root = metadata.workspace_root.as_std_path();
    let mut out = Vec::new();
    for pkg in metadata.workspace_packages() {
        let name = pkg.name.as_str();
        if excluded.iter().any(|e| e == name) {
            continue;
        }
        let Some(package_root) = pkg.manifest_path.parent() else {
            continue;
        };
        let root = if include_tests {
            package_root.as_std_path().to_path_buf()
        } else {
            package_root.join("src").into_std_path_buf()
        };
        if root.is_dir() {
            out.push(
                root.strip_prefix(workspace_root)
                    .unwrap_or(&root)
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
        }
    }
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_scan_omits_test_paths_but_strict_scan_keeps_them() {
        let temp = tempfile::tempdir().expect("tempdir");
        let src = temp.path().join("crate/src");
        let tests = src.join("tests");
        fs::create_dir_all(&tests).expect("create test directory");
        fs::write(src.join("lib.rs"), "struct Production;").expect("write production source");
        fs::write(tests.join("fixture.rs"), "struct Fixture;").expect("write test source");
        let roots = vec!["crate/src".to_string()];

        let production = source_files(temp.path(), &roots, false).expect("production files");
        let strict = source_files(temp.path(), &roots, true).expect("strict files");

        assert_eq!(production.len(), 1);
        assert_eq!(strict.len(), 2);
    }

    #[test]
    fn similarity_can_include_project_default_exclusions() {
        let command = SimilarityArgs::augment_args(clap::Command::new("similarity"));
        let matches = command.try_get_matches_from([
            "similarity",
            "--include-default-excluded",
            "--profile",
            "strict",
        ]);

        assert!(
            matches.is_ok(),
            "complete profile override should parse: {matches:?}"
        );
    }
}

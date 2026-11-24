use std::{
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, ensure};
use cargo_metadata::{Metadata, MetadataCommand};
use tempfile::{Builder, TempDir};
use toml::Value;

use super::{
    Log, Request,
    command::{PREFLIGHT_STRIP, sha256},
};
use crate::Ctx;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Target {
    pub(super) bench: String,
    pub(super) package: String,
    pub(super) features: Vec<String>,
}

impl Target {
    pub(super) fn id(&self) -> String {
        format!(
            "{}:{}:{}",
            self.package,
            self.bench,
            self.features.join(",")
        )
    }
}

pub(super) fn targets(metadata: &Metadata) -> Vec<Target> {
    let mut targets = Vec::new();
    for package in metadata.workspace_packages() {
        for target in package.targets.iter().filter(|target| target.is_bench()) {
            let mut features = target.required_features.clone();
            features.sort();
            targets.push(Target {
                features,
                bench: target.name.clone(),
                package: package.name.to_string(),
            });
        }
    }
    targets.sort_by_key(Target::id);
    targets
}

pub(super) fn target_parts(selector: &str) -> Result<Vec<&str>> {
    let parts: Vec<_> = selector.split(':').collect();
    ensure!(
        (2..=3).contains(&parts.len()) && !parts[0].is_empty() && !parts[1].is_empty(),
        "bench target must be package:bench[:required-features]"
    );
    Ok(parts)
}

pub(super) fn select(metadata: &Metadata, selector: &str) -> Result<Vec<Target>> {
    let available = targets(metadata);
    if selector.is_empty() {
        return Ok(available);
    }
    let parts = target_parts(selector)?;
    let target = available
        .into_iter()
        .find(|target| target.package == parts[0] && target.bench == parts[1])
        .with_context(|| format!("unknown workspace bench target {selector}"))?;
    if let Some(features) = parts.get(2) {
        let mut requested: Vec<_> = features
            .split(',')
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .collect();
        requested.sort();
        ensure!(
            requested == target.features,
            "{selector}: features must match manifest-required {}",
            target.features.join(",")
        );
    }
    Ok(vec![target])
}

pub(super) struct Sources {
    pub(super) base: Option<PathBuf>,
    pub(super) head: PathBuf,
    pub(super) scratch: TempDir,
    _checkouts: Option<TempDir>,
}

impl Sources {
    pub(super) fn prepare(
        ctx: &Ctx,
        request: &Request,
        preflight: bool,
        log: &mut Log,
    ) -> Result<Self> {
        let target = ctx.metadata()?.target_directory.as_std_path();
        fs::create_dir_all(target)?;
        let scratch = Builder::new().prefix("bench-stage-").tempdir_in(target)?;
        let repository = if request.head.is_empty() && request.base.is_empty() {
            None
        } else {
            let manifest: Value =
                toml::from_str(&fs::read_to_string(ctx.root.join("Cargo.toml"))?)?;
            Some(
                manifest
                    .get("workspace")
                    .and_then(|value| value.get("package"))
                    .and_then(|value| value.get("repository"))
                    .and_then(Value::as_str)
                    .context("source refs require workspace.package.repository")?
                    .to_owned(),
            )
        };
        let checkouts = repository
            .as_ref()
            .map(|_| Builder::new().prefix("bench-source-").tempdir())
            .transpose()?;
        let head = if request.head.is_empty() {
            ctx.root.clone()
        } else {
            checkout(
                checkouts
                    .as_ref()
                    .context("benchmark checkout directory")?
                    .path()
                    .join("head"),
                repository.as_deref().context("benchmark repository")?,
                &request.head,
                log,
            )?
        };
        let base = if request.base.is_empty() {
            None
        } else {
            Some(checkout(
                checkouts
                    .as_ref()
                    .context("benchmark checkout directory")?
                    .path()
                    .join("base"),
                repository.as_deref().context("benchmark repository")?,
                &request.base,
                log,
            )?)
        };
        for (name, root, requested) in [
            ("head", Some(&head), &request.head),
            ("base", base.as_ref(), &request.base),
        ] {
            if let Some(root) = root {
                let output = log.output(
                    Command::new("git")
                        .args(["rev-parse", "HEAD"])
                        .current_dir(root),
                    false,
                )?;
                let actual = String::from_utf8(output.stdout)?;
                let actual = actual.trim();
                ensure!(
                    !preflight || actual == requested,
                    "preflight {name} staged {actual}, expected {requested}"
                );
                log.line(&format!("{name} commit {actual}"))?;
            }
        }
        Ok(Self {
            base,
            head,
            scratch,
            _checkouts: checkouts,
        })
    }
}

fn checkout(path: PathBuf, repository: &str, source: &str, log: &mut Log) -> Result<PathBuf> {
    ensure!(
        !source.starts_with('-') && !source.chars().any(char::is_control),
        "invalid benchmark ref"
    );
    log.output(
        Command::new("git").arg("init").arg("--quiet").arg(&path),
        false,
    )?;
    log.output(
        Command::new("git")
            .args([
                "fetch",
                "--quiet",
                "--no-tags",
                "--depth=1",
                repository,
                source,
            ])
            .current_dir(&path),
        false,
    )?;
    log.output(
        Command::new("git")
            .args(["checkout", "--quiet", "--detach", "FETCH_HEAD"])
            .current_dir(&path),
        false,
    )?;
    Ok(path)
}

pub(super) fn metadata(root: &Path, cargo: &str) -> Result<Metadata> {
    MetadataCommand::new()
        .cargo_path(cargo)
        .current_dir(root)
        .no_deps()
        .other_options(vec!["--locked".to_owned()])
        .exec()
        .context("read benchmark source metadata")
}

pub(super) fn build_provenance(
    ctx: &Ctx,
    sources: &Sources,
    digest: &str,
    log: &mut Log,
) -> Result<String> {
    let controller = log.output(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&ctx.root),
        true,
    )?;
    let mut proof = format!(
        "controller {}\nOS {}\nARCH {}\ndeclaration SHA-256 {digest}\n{PREFLIGHT_STRIP}=none\n",
        std::str::from_utf8(&controller.stdout)?.trim(),
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    let base = sources.base.as_deref().context("preflight base source")?;
    for (arm, root) in [("base", base), ("head", &sources.head)] {
        let commit = log.output(
            Command::new("git")
                .args(["cat-file", "commit", "HEAD"])
                .current_dir(root),
            true,
        )?;
        let header = std::str::from_utf8(&commit.stdout)?
            .split_once("\n\n")
            .context("raw commit header")?
            .0;
        writeln!(proof, "{arm} raw commit header\n{header}")?;
        for (tool, arg) in [("rustc", "-vV"), ("cargo", "-V")] {
            let output = log.output(
                Command::new(ctx.config.tools.program(tool))
                    .arg(arg)
                    .current_dir(root),
                true,
            )?;
            writeln!(
                proof,
                "{arm} {}",
                std::str::from_utf8(&output.stdout)?.trim()
            )?;
        }
        let hash = sha256(&fs::read(root.join("Cargo.lock"))?);
        writeln!(proof, "{arm} Cargo.lock SHA-256 {hash}")?;
    }
    for name in ["RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "RUSTC_WRAPPER"] {
        writeln!(proof, "{name}={:?}", std::env::var_os(name))?;
    }
    for profile in ["BENCH", "RELEASE"] {
        for field in [
            "CODEGEN_UNITS",
            "DEBUG",
            "DEBUG_ASSERTIONS",
            "INCREMENTAL",
            "LTO",
            "OPT_LEVEL",
            "OVERFLOW_CHECKS",
            "PANIC",
            "RPATH",
            "SPLIT_DEBUGINFO",
            "STRIP",
        ] {
            let name = format!("CARGO_PROFILE_{profile}_{field}");
            if name != PREFLIGHT_STRIP
                && let Some(value) = std::env::var_os(&name)
            {
                writeln!(proof, "{name}={value:?}")?;
            }
        }
    }
    Ok(proof)
}

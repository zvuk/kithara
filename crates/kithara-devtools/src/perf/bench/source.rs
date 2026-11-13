use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, ensure};
use cargo_metadata::{Metadata, MetadataCommand};
use tempfile::{Builder, TempDir};

use super::{Deployment, Log, Request};
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

pub(super) fn select(metadata: &Metadata, selector: &str) -> Result<Vec<Target>> {
    let available = targets(metadata);
    if selector.is_empty() {
        return Ok(available);
    }
    let parts: Vec<_> = selector.split(':').collect();
    ensure!(
        (2..=3).contains(&parts.len()),
        "bench target must be package:bench[:required-features]"
    );
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
}

impl Sources {
    pub(super) fn prepare(ctx: &Ctx, request: &Request, log: &mut Log) -> Result<Self> {
        let target = ctx.metadata()?.target_directory.as_std_path();
        fs::create_dir_all(target)?;
        let scratch = Builder::new().prefix("bench-stage-").tempdir_in(target)?;
        let repository = if request.head.is_empty() && request.base.is_empty() {
            None
        } else {
            Some(Deployment::load(ctx)?.repository)
        };
        let head = if request.head.is_empty() {
            ctx.root.clone()
        } else {
            checkout(
                scratch.path().join("head"),
                repository.as_deref().context("benchmark repository")?,
                &request.head,
                log,
            )?
        };
        let base = if request.base.is_empty() {
            None
        } else {
            Some(checkout(
                scratch.path().join("base"),
                repository.as_deref().context("benchmark repository")?,
                &request.base,
                log,
            )?)
        };
        for (name, root) in [("head", Some(&head)), ("base", base.as_ref())] {
            if let Some(root) = root {
                let output = log.output(
                    Command::new("git")
                        .args(["rev-parse", "HEAD"])
                        .current_dir(root),
                    false,
                )?;
                log.line(&format!(
                    "{name} commit {}",
                    String::from_utf8_lossy(&output.stdout).trim()
                ))?;
            }
        }
        Ok(Self {
            base,
            head,
            scratch,
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

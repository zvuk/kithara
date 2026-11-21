use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context, Result};
use cargo_metadata::{Metadata, MetadataCommand};
use tracing::info;

use super::{
    draft::DraftScope, request::TestRequest, resolve::ResolvedLane, unit_graph::UnitGraph,
};
use crate::{common::project::TestRunner, verdict::ChildFailure};

pub(super) fn admit(
    lane: ResolvedLane,
    scope: &DraftScope,
    request: &TestRequest,
    root: &Path,
) -> Result<ResolvedLane> {
    let Some(packages) = eligible_packages(&lane, scope, request) else {
        return Ok(lane);
    };
    if !lane.cargo.workspace && packages == lane.cargo.packages.iter().cloned().collect() {
        return Ok(lane);
    }
    let output = native_cargo(&lane)?
        .args(["metadata", "--format-version=1", "--no-deps", "--locked"])
        .current_dir(root)
        .output()
        .context("resolve Draft build packages")?;
    if !output.status.success() {
        return Err(ChildFailure::captured(
            "resolve Draft build packages".to_owned(),
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ));
    }
    let metadata: Metadata = serde_json::from_slice(&output.stdout)?;
    if lane.features.iter().any(|feature| {
        !metadata.workspace_packages().into_iter().any(|package| {
            packages.contains(package.name.as_str()) && package.features.contains_key(feature)
        })
    }) {
        info!(
            lane = lane.lane,
            "Draft retains the full Cargo build: feature not declared by selected packages"
        );
        return Ok(lane);
    }
    let members: BTreeSet<_> = metadata
        .workspace_packages()
        .into_iter()
        .map(|package| package.name.to_string())
        .collect();
    let original: BTreeSet<_> = if lane.cargo.workspace {
        members
            .difference(&lane.cargo.exclude.iter().cloned().collect())
            .cloned()
            .collect()
    } else {
        lane.cargo.packages.iter().cloned().collect()
    };
    if packages == original {
        return Ok(lane);
    }
    let Some(cargo) = graph_cargo(&lane)? else {
        return Ok(lane);
    };
    let full = graph(&lane, root, &cargo)?;
    let ids = metadata
        .workspace_packages()
        .into_iter()
        .filter(|package| packages.contains(package.name.as_str()))
        .map(|package| package.id.to_string())
        .collect();
    if !full.has_selected_roots(&ids) {
        info!(
            lane = lane.lane,
            "Draft retains the full Cargo build: no matching active roots"
        );
        return Ok(lane);
    }
    let removed: BTreeSet<_> = metadata
        .workspace_packages()
        .into_iter()
        .filter(|package| {
            original.contains(package.name.as_str()) && !packages.contains(package.name.as_str())
        })
        .map(|package| package.id.to_string())
        .collect();
    if !full.closure_packages(&ids).is_disjoint(&removed) {
        info!(
            lane = lane.lane,
            "Draft retains the full Cargo build: dependency primary-package environment would change"
        );
        return Ok(lane);
    }
    let expected = full.selected_roots(&ids)?;
    let mut candidate = lane.clone();
    candidate.cargo.workspace = false;
    candidate.cargo.exclude.clear();
    candidate.cargo.packages = packages.into_iter().collect();
    let narrowed = graph(&candidate, root, &cargo)?;
    if !narrowed.has_selected_roots(&ids) || narrowed.all_roots()? != expected {
        info!(
            lane = lane.lane,
            "Draft retains the full Cargo build: selected unit graph differs"
        );
        return Ok(lane);
    }
    compile(&candidate, &narrowed, root)?;
    info!(lane = lane.lane, packages = ?candidate.cargo.packages, "Draft package build admitted");
    Ok(candidate)
}

fn eligible_packages(
    lane: &ResolvedLane,
    scope: &DraftScope,
    request: &TestRequest,
) -> Option<BTreeSet<String>> {
    let reason = if !matches!(lane.runner, TestRunner::Nextest(_)) {
        Some("native Cargo runner")
    } else if request.narrow.is_some() || !supported_caller(&request.passthrough) {
        Some("manual filter or unsupported caller options")
    } else if lane.cargo.lib || !lane.cargo.tests.is_empty() {
        Some("explicit Cargo target selectors are not qualified for package builds")
    } else if lane.features.iter().any(|feature| feature.contains('/')) {
        Some("package-qualified feature requests need additional proof")
    } else {
        None
    };
    if let Some(reason) = reason {
        info!(
            lane = lane.lane,
            reason, "Draft retains the full Cargo build"
        );
        return None;
    }
    let Some(packages) = scope.package_selection(&lane.cargo) else {
        info!(
            lane = lane.lane,
            "Draft retains the full Cargo build: arbitrary PR filter"
        );
        return None;
    };
    Some(packages)
}

fn graph_cargo(lane: &ResolvedLane) -> Result<Option<PathBuf>> {
    let Some(nightly) = lane
        .env
        .get("KITHARA_NIGHTLY_TOOLCHAIN")
        .cloned()
        .or_else(|| std::env::var("KITHARA_NIGHTLY_TOOLCHAIN").ok())
        .filter(|pin| pin.starts_with("nightly-") && pin.len() == "nightly-YYYY-MM-DD".len())
    else {
        info!(
            lane = lane.lane,
            "Draft retains the full Cargo build: no pinned graph toolchain"
        );
        return Ok(None);
    };
    if !lane
        .env
        .get("RUSTUP_TOOLCHAIN")
        .cloned()
        .or_else(|| std::env::var("RUSTUP_TOOLCHAIN").ok())
        .is_some_and(|pin| !pin.trim().is_empty())
    {
        info!(
            lane = lane.lane,
            "Draft retains the full Cargo build: native toolchain is not explicit"
        );
        return Ok(None);
    }
    let output = Command::new("rustup")
        .args(["which", "--toolchain", &nightly, "cargo"])
        .envs(&lane.env)
        .output()
        .context("locate pinned graph Cargo")?;
    if !output.status.success() {
        return Err(ChildFailure::captured(
            "locate pinned graph Cargo".to_owned(),
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ));
    }
    let cargo = String::from_utf8(output.stdout).context("graph Cargo path is not UTF-8")?;
    Ok(Some(PathBuf::from(cargo.trim())))
}

fn compile(lane: &ResolvedLane, graph: &UnitGraph, root: &Path) -> Result<()> {
    let mut command = native_cargo(lane)?;
    command
        .args(["test", "--no-run", "--locked", "--message-format=json"])
        .args(lane.cargo_args("--profile", false))
        .current_dir(root)
        .stderr(Stdio::inherit());
    let output = command
        .output()
        .context("compile admitted Draft packages")?;
    if !output.status.success() {
        return Err(ChildFailure::inherited(
            "compile admitted Draft packages".to_owned(),
            output.status.code(),
        ));
    }
    graph.verify_artifacts(&output.stdout)
}

fn graph(lane: &ResolvedLane, root: &Path, cargo: &Path) -> Result<UnitGraph> {
    // An absolute nightly Cargo keeps the lane's native RUSTUP_TOOLCHAIN for rustc.
    let mut command = Command::new(cargo);
    command
        .envs(&lane.env)
        .env_remove("RUSTC_WRAPPER")
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .args([
            "test",
            "--no-run",
            "--unit-graph",
            "-Z",
            "unstable-options",
            "--locked",
        ])
        .args(lane.cargo_args("--profile", false))
        .current_dir(root);
    let output = command.output().context("resolve Draft Cargo unit graph")?;
    if !output.status.success() {
        return Err(ChildFailure::captured(
            "resolve Draft Cargo unit graph".to_owned(),
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ));
    }
    UnitGraph::parse(&output.stdout)
}

fn native_cargo(lane: &ResolvedLane) -> Result<Command> {
    let mut command = match lane.env.get("CARGO") {
        Some(cargo) => Command::new(cargo),
        None => Command::new(
            which::which(MetadataCommand::new().cargo_command().get_program())
                .context("resolve Cargo for the Draft build")?,
        ),
    };
    command.envs(&lane.env);
    Ok(command)
}

fn supported_caller(args: &[String]) -> bool {
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--profile" | "-P" => {
                if args.next().is_none() {
                    return false;
                }
            }
            "--no-capture" | "--no-fail-fast" | "--fail-fast" => {}
            arg if arg.starts_with("--profile=") => {}
            _ => return false,
        }
    }
    true
}

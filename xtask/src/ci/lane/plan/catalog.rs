use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use kithara_devtools::common::project::ProjectConfig;
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use crate::{
    ci::run::Lane,
    config::{CiCheckKind, CiCheckSemantics, CiExtraCheck, CiLaneConfig, CiProjectConfig},
    consts,
};

#[derive(Serialize)]
pub(super) struct CheckDefinition {
    pub(super) kind: CiCheckKind,
    pub(super) platforms: BTreeSet<String>,
    pub(super) kinds: BTreeSet<String>,
    pub(super) capabilities: BTreeSet<String>,
    pub(super) needs: BTreeSet<String>,
    pub(super) parameters: BTreeSet<String>,
    pub(super) timeout_minutes: Option<u32>,
    pub(super) path_globs: Vec<String>,
    pub(super) contract: Value,
}

#[derive(Serialize)]
pub(super) struct Catalog {
    #[serde(skip)]
    pub(super) root: PathBuf,
    #[serde(skip)]
    pub(super) project: ProjectConfig,
    pub(super) checks: BTreeMap<String, CheckDefinition>,
    pub(super) bindings: BTreeMap<String, BTreeSet<String>>,
    pub(super) build_and_test: Value,
    pub(super) pins: Value,
}

impl Catalog {
    pub(super) fn load(root: &Path, config: &CiProjectConfig) -> Result<Self> {
        let mut checks = BTreeMap::new();
        for (name, lane) in &config.lanes {
            let semantics = lane
                .semantics
                .as_ref()
                .with_context(|| format!("unclassified CI lane: {name}"))?;
            validate_semantics(semantics)?;
            ensure!(
                !Lane::RESIDUAL.iter().any(|lane| lane.name() == name),
                "declared lane shadows residual owner: {name}"
            );
            let mut contract = serde_json::to_value(lane)?;
            let object = contract
                .as_object_mut()
                .context("lane contract must be an object")?;
            for field in [
                "kinds_gitlab",
                "kinds_github",
                "github_runner",
                "github_runner_main",
            ] {
                object.remove(field);
            }
            let kinds = semantics.automatic_kinds.as_ref().map_or_else(
                || {
                    lane.kinds_gitlab
                        .iter()
                        .chain(&lane.kinds_github)
                        .map(|kind| normalize_kind(kind).to_owned())
                        .collect()
                },
                |kinds| {
                    kinds
                        .iter()
                        .map(|kind| normalize_kind(kind).to_owned())
                        .collect()
                },
            );
            checks.insert(
                name.clone(),
                CheckDefinition {
                    kind: semantics.kind,
                    platforms: lane.os.iter().cloned().collect(),
                    kinds,
                    capabilities: semantics.capabilities.clone(),
                    needs: lane.needs.iter().cloned().collect(),
                    parameters: BTreeSet::new(),
                    timeout_minutes: Some(lane.timeout_minutes),
                    path_globs: lane.path_globs.clone(),
                    contract,
                },
            );
        }
        let residuals: BTreeSet<_> = Lane::RESIDUAL
            .iter()
            .map(|lane| lane.name().to_owned())
            .collect();
        ensure!(
            config
                .planning
                .residuals
                .keys()
                .cloned()
                .collect::<BTreeSet<_>>()
                == residuals,
            "planning must classify every residual lane exactly once"
        );
        for (family, entries) in [
            ("ci-run", &config.planning.residuals),
            ("dedicated", &config.planning.dedicated),
        ] {
            for (name, extra) in entries {
                ensure!(!checks.contains_key(name), "shadowed check ID: {name}");
                validate_extra(name, extra)?;
                let sources = extra
                    .sources
                    .iter()
                    .map(|path| Ok((path.clone(), digest(&source(root, path)?))))
                    .collect::<Result<BTreeMap<_, _>>>()?;
                checks.insert(name.clone(), CheckDefinition {
                    kind: extra.semantics.kind,
                    platforms: extra.platforms.clone(),
                    kinds: extra.kinds.iter().map(|kind| normalize_kind(kind).to_owned()).collect(),
                    capabilities: extra.semantics.capabilities.clone(),
                    needs: extra.needs.clone(),
                    parameters: extra.parameters.clone(),
                    timeout_minutes: extra.timeout_minutes,
                    path_globs: Vec::new(),
                    contract: json!({"family": family, "owner": extra.owner, "semantics": extra.semantics, "sources": sources}),
                });
            }
        }
        for (id, check) in &checks {
            ensure!(
                check
                    .needs
                    .iter()
                    .all(|need| checks.contains_key(need) && need != id),
                "unknown or self dependency for {id}"
            );
        }
        validate_bindings(root, config, &checks)?;
        let project_text = fs::read_to_string(root.join(".config/xtask.toml"))?;
        let project_config: ProjectConfig = toml::from_str(&project_text)?;
        let mut project: Value = toml::from_str(&project_text)?;
        if let Some(ext) = project.get_mut("ext").and_then(Value::as_object_mut)
            && let Some(mut ci) = ext.remove("ci")
        {
            if let Some(fields) = ci.as_object_mut() {
                fields.remove("lanes");
                fields.remove("planning");
            }
            ext.insert("ci_acceptance".to_owned(), ci);
        }
        project["nextest_contract"] =
            toml::from_str(&fs::read_to_string(root.join(".config/nextest.toml"))?)?;
        let pins = toml::from_str(&fs::read_to_string(root.join(&config.pins))?)?;
        Ok(Self {
            root: root.to_owned(),
            project: project_config,
            checks,
            bindings: config.planning.bindings.clone(),
            build_and_test: project,
            pins,
        })
    }

    pub(super) fn fingerprint(&self) -> Result<String> {
        Ok(digest(&serde_json::to_vec(self)?))
    }
}

pub(super) fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

pub(super) fn normalize_kind(kind: &str) -> &str {
    if kind == "quarantine" {
        "merge-request"
    } else {
        kind
    }
}

fn source(root: &Path, relative: &str) -> Result<Vec<u8>> {
    let path = Path::new(relative);
    ensure!(
        !path.is_absolute()
            && path
                .components()
                .all(|part| matches!(part, Component::Normal(_))),
        "source contracts require project-relative files"
    );
    let path = root.join(path).canonicalize()?;
    ensure!(
        path.starts_with(root.canonicalize()?),
        "contract source escapes the project"
    );
    fs::read(path).map_err(Into::into)
}

fn validate_semantics(semantics: &CiCheckSemantics) -> Result<()> {
    ensure!(
        !semantics.acceptance.is_empty(),
        "every check must declare acceptance/report policy"
    );
    ensure!(
        semantics
            .automatic_kinds
            .iter()
            .flatten()
            .all(|kind| consts::PIPELINE_KINDS.contains(&kind.as_str())),
        "unknown automatic check context"
    );
    ensure!(
        semantics.capabilities.iter().all(|value| !value.is_empty()
            && value
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"-_:".contains(&c))),
        "invalid abstract check capability"
    );
    Ok(())
}

fn validate_extra(name: &str, extra: &CiExtraCheck) -> Result<()> {
    validate_semantics(&extra.semantics)?;
    ensure!(
        !extra.owner.is_empty() && !extra.sources.is_empty(),
        "{name} must reference its existing implementation owner"
    );
    ensure!(
        !extra.platforms.is_empty()
            && extra
                .platforms
                .iter()
                .all(|os| matches!(os.as_str(), "linux" | "macos" | "windows")),
        "{name} has invalid platform contracts"
    );
    ensure!(
        extra
            .kinds
            .iter()
            .all(|kind| consts::PIPELINE_KINDS.contains(&kind.as_str())),
        "{name} has unknown scope"
    );
    ensure!(
        extra.timeout_minutes != Some(0),
        "{name} has a zero timeout"
    );
    Ok(())
}

fn validate_bindings(
    root: &Path,
    config: &CiProjectConfig,
    checks: &BTreeMap<String, CheckDefinition>,
) -> Result<()> {
    let mut actual = BTreeSet::new();
    for directory in [".github/workflows", ".gitlab/ci"] {
        for entry in fs::read_dir(root.join(directory))? {
            let path = entry?.path();
            if path.extension().is_none_or(|ext| ext != "yml") {
                continue;
            }
            let relative = path
                .strip_prefix(root)?
                .to_string_lossy()
                .replace('\\', "/");
            workflow_jobs(root, &relative, &mut actual)?;
        }
    }
    workflow_jobs(root, ".gitlab-ci.yml", &mut actual)?;
    ensure!(
        config
            .planning
            .bindings
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>()
            == actual,
        "provider entrypoint inventory has unclassified or stale declarations"
    );
    for (entrypoint, targets) in &config.planning.bindings {
        ensure!(
            !targets.is_empty(),
            "entrypoint has no semantic owner: {entrypoint}"
        );
        for target in targets {
            let catalog_role = target
                .strip_prefix("catalog:")
                .is_some_and(|role| role == "all" || consts::LANE_ROLES.contains(&role));
            ensure!(
                checks.contains_key(target) || catalog_role,
                "entrypoint {entrypoint} names unknown contract {target}"
            );
        }
    }
    Ok(())
}

fn workflow_jobs(root: &Path, path: &str, output: &mut BTreeSet<String>) -> Result<()> {
    let value: serde_yaml_ng::Value = serde_yaml_ng::from_slice(&source(root, path)?)?;
    let jobs = if path.starts_with(".github/") {
        &value["jobs"]
    } else {
        &value
    };
    let Some(mapping) = jobs.as_mapping() else {
        return Ok(());
    };
    for key in mapping.keys().filter_map(serde_yaml_ng::Value::as_str) {
        if key.starts_with('.')
            || (!path.starts_with(".github/")
                && [
                    "workflow",
                    "stages",
                    "variables",
                    "default",
                    "include",
                    "image",
                    "cache",
                    "before_script",
                    "after_script",
                    "services",
                ]
                .contains(&key))
        {
            continue;
        }
        output.insert(format!("{path}::{key}"));
    }
    Ok(())
}

pub(super) fn commands(lane: &CiLaneConfig, kind: &str) -> Value {
    Value::Array(lane.steps.iter().map(|step| json!({"program":step.program.as_deref().unwrap_or(&lane.program),"args":step.args_in(kind),"env":step.env,"rebuild_check":step.rebuild_check})).collect())
}

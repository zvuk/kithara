use std::{
    collections::BTreeSet,
    fs,
    path::{Component, Path},
};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};

use super::lanes::RunPaths;
use crate::{
    common::project::StressRenderBudgets,
    consts,
    junit::CaseTiming,
    stress_report::{parse_timestamp_ms, read_bounded_utf8, read_primary_evidence},
    test::ResolvedLane,
};

pub(super) type LaneCases = (String, Vec<CaseTiming>);

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct MatrixPlan {
    pub(super) format_version: u32,
    pub(super) run_id: String,
    pub(super) commit: String,
    pub(super) host: String,
    pub(super) platform: String,
    pub(super) architecture: String,
    pub(super) toolchain: String,
    pub(super) nextest: String,
    pub(super) nextest_config_sha256: String,
    pub(super) primary_lane: String,
    pub(super) repeats: u32,
    pub(super) lanes: Vec<PlannedLane>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PlannedLane {
    pub(super) name: String,
    pub(super) flash: bool,
    pub(super) runner: ResolvedLane,
    pub(super) arguments: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RepeatMeta {
    pub(super) exit_code: Option<i32>,
    pub(super) commit: String,
    pub(super) lane: String,
    pub(super) run_id: String,
    pub(super) features: Vec<String>,
    pub(super) duration_secs: f64,
    pub(super) preparation_secs: f64,
    pub(super) repeat: u32,
    pub(super) started_unix_ms: u64,
    pub(super) finished_unix_ms: u64,
    pub(super) inventory_sha256: String,
    pub(super) junit_sha256: Option<String>,
    pub(super) nextest_run_id: Option<String>,
    pub(super) nextest_config_sha256: String,
}

pub(super) fn component(value: &str) -> Result<()> {
    let mut parts = Path::new(value).components();
    ensure!(
        matches!(parts.next(), Some(Component::Normal(_))) && parts.next().is_none(),
        "measurement name must be one normal path component: {value}"
    );
    Ok(())
}

pub(super) fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    fs::write(
        path,
        serde_json::to_vec_pretty(value).context("serialize measurement metadata")?,
    )
    .with_context(|| format!("write measurement metadata {}", path.display()))
}

pub(super) fn digest(path: &Path, limit: u64, label: &str) -> Result<String> {
    let text = read_bounded_utf8(path, limit, label)?;
    Ok(hex::encode(Sha256::digest(text.as_bytes())))
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let limit = u64::try_from(consts::MAX_MANIFEST_BYTES).context("metadata limit")?;
    let text = read_bounded_utf8(path, limit, "matrix metadata")?;
    serde_json::from_str(&text).with_context(|| format!("parse matrix metadata {}", path.display()))
}

pub(super) fn validate_plan(plan: &MatrixPlan, run_id: &str) -> Result<()> {
    component(run_id)?;
    ensure!(
        plan.format_version == 1,
        "unsupported matrix evidence format"
    );
    ensure!(plan.run_id == run_id, "matrix plan run identity mismatch");
    ensure!(plan.repeats > 0, "matrix plan has no repetitions");
    ensure!(
        plan.commit.len() == 40 && plan.commit.bytes().all(|b| b.is_ascii_hexdigit()),
        "matrix plan has no exact source revision"
    );
    for value in [
        &plan.host,
        &plan.platform,
        &plan.architecture,
        &plan.toolchain,
        &plan.nextest,
    ] {
        ensure!(
            !value.trim().is_empty(),
            "matrix plan provenance is incomplete"
        );
    }
    ensure!(!plan.lanes.is_empty(), "matrix plan contains no lanes");
    let mut names = BTreeSet::new();
    for lane in &plan.lanes {
        component(&lane.name)?;
        lane.runner.validate()?;
        let mode = if lane.flash { "on" } else { "off" };
        ensure!(
            lane.name == format!("flash-{mode}-{}", lane.runner.backend),
            "matrix lane does not match its requested mode/backend"
        );
        ensure!(
            names.insert(lane.name.clone()),
            "duplicate planned matrix lane"
        );
        ensure!(
            lane.arguments
                .last()
                .is_some_and(|arg| arg == "--retries=0")
                && lane.arguments.iter().any(|arg| arg == "--stress-count=1"),
            "matrix invocation must record one iteration with retries disabled"
        );
    }
    ensure!(
        names.contains(&plan.primary_lane),
        "matrix primary lane was not measured"
    );
    Ok(())
}

fn directories(path: &Path) -> Result<BTreeSet<String>> {
    let mut names = BTreeSet::new();
    for entry in fs::read_dir(path).with_context(|| format!("read {}", path.display()))? {
        let entry = entry.context("read measurement directory entry")?;
        let kind = entry.file_type().context("read measurement entry type")?;
        ensure!(
            !kind.is_symlink(),
            "measurement directory contains a symlink"
        );
        if kind.is_dir() {
            names.insert(
                entry
                    .file_name()
                    .into_string()
                    .map_err(|_| anyhow::anyhow!("measurement directory name is not UTF-8"))?,
            );
        }
    }
    Ok(names)
}

fn validate_receipt(
    meta: &RepeatMeta,
    plan: &MatrixPlan,
    lane: &PlannedLane,
    repeat: u32,
) -> Result<()> {
    ensure!(
        meta.exit_code == Some(0),
        "matrix invocation did not exit successfully"
    );
    ensure!(
        meta.run_id == plan.run_id && meta.commit == plan.commit,
        "matrix receipt source/run identity mismatch"
    );
    ensure!(
        meta.lane == lane.name && meta.repeat == repeat && meta.features == lane.runner.features,
        "matrix receipt lane/repetition/features mismatch"
    );
    ensure!(
        meta.duration_secs.is_finite()
            && meta.duration_secs >= 0.0
            && meta.preparation_secs.is_finite()
            && meta.preparation_secs >= 0.0
            && meta.finished_unix_ms >= meta.started_unix_ms,
        "matrix receipt has invalid execution timing"
    );
    Ok(())
}

pub(super) fn read_matrix(paths: &RunPaths, run_id: &str) -> Result<(MatrixPlan, Vec<LaneCases>)> {
    component(run_id)?;
    let plan: MatrixPlan = read_json(&paths.run_dir.join("matrix.json"))?;
    validate_plan(&plan, run_id)?;
    ensure!(
        digest(
            &paths.run_dir.join("nextest.toml"),
            consts::MAX_MANIFEST_BYTES as u64,
            "nextest configuration"
        )? == plan.nextest_config_sha256,
        "matrix nextest configuration digest mismatch"
    );
    let expected_lanes: BTreeSet<_> = plan.lanes.iter().map(|lane| lane.name.clone()).collect();
    ensure!(
        directories(&paths.matrix_dir())? == expected_lanes,
        "matrix lane directories differ from the requested plan"
    );
    let mut execution_ids = BTreeSet::new();
    let mut runs = Vec::new();
    for lane in &plan.lanes {
        let observed_repeats = directories(&paths.matrix_dir().join(&lane.name))?;
        ensure!(
            u64::try_from(observed_repeats.len()).context("matrix repetition count")?
                == u64::from(plan.repeats)
                && (1..=plan.repeats).all(|n| observed_repeats.contains(&format!("rep{n}"))),
            "matrix repetitions differ from the plan for {}",
            lane.name
        );
        let mut selected = None;
        for repeat in 1..=plan.repeats {
            let dir = paths.repeat_dir(&lane.name, repeat);
            let meta: RepeatMeta = read_json(&dir.join("meta.json"))?;
            validate_receipt(&meta, &plan, lane, repeat)?;
            ensure!(
                digest(
                    &dir.join("nextest.toml"),
                    consts::MAX_MANIFEST_BYTES as u64,
                    "invocation configuration"
                )? == meta.nextest_config_sha256,
                "matrix invocation configuration digest mismatch"
            );
            let inventory_path = dir.join("inventory.json");
            let junit_path = dir.join("junit.xml");
            ensure!(
                digest(
                    &inventory_path,
                    consts::MAX_INVENTORY_BYTES,
                    "matrix inventory"
                )? == meta.inventory_sha256,
                "matrix inventory digest mismatch"
            );
            ensure!(
                Some(digest(
                    &junit_path,
                    consts::MAX_JUNIT_BYTES,
                    "matrix JUnit"
                )?) == meta.junit_sha256,
                "matrix JUnit digest mismatch"
            );
            let (inventory, report) = read_primary_evidence(
                &inventory_path,
                &junit_path,
                1,
                &StressRenderBudgets::default(),
            )?;
            if let Some(previous) = &selected {
                ensure!(
                    previous == &inventory,
                    "matrix selection changed between repetitions"
                );
            } else {
                selected = Some(inventory);
            }
            let execution = report
                .run_id
                .as_ref()
                .context("matrix JUnit execution identity")?;
            ensure!(
                Some(execution) == meta.nextest_run_id.as_ref()
                    && execution_ids.insert(execution.clone()),
                "matrix execution identity mismatched or reused"
            );
            for timestamp in std::iter::once(report.timestamp.as_deref())
                .chain(report.cases.iter().map(|case| case.timestamp.as_deref()))
            {
                let observed = timestamp
                    .and_then(parse_timestamp_ms)
                    .context("matrix JUnit timestamp")?;
                ensure!(
                    (meta.started_unix_ms..=meta.finished_unix_ms).contains(&observed),
                    "matrix JUnit lies outside its recorded execution interval"
                );
            }
            runs.push((lane.name.clone(), report.cases));
        }
    }
    Ok((plan, runs))
}

#[cfg(test)]
#[path = "evidence_tests.rs"]
mod tests;

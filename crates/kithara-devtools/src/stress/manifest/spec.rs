use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    time::SystemTime,
};

use anyhow::{Context, Result, bail, ensure};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};

use super::{
    super::{pressure::SCHEMA as PRESSURE_SCHEMA, system::SystemSnapshot},
    time::format_timestamp,
};
use crate::{common::project::StressEvidenceConfig, test::ConfiguredLane};

struct Consts;

impl Consts {
    const MANIFEST_READ_LIMIT: u64 = 1_048_577;
    const MANIFEST_SCHEMA: u32 = 4;
    const MAX_MANIFEST_BYTES: usize = 1_048_576;
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::stress) struct Manifest {
    pub(in crate::stress) build: BuildSnapshot,
    pub(in crate::stress) runner: ConfiguredLane,
    pub(in crate::stress) config: ManifestConfig,
    pub(in crate::stress) policy: PolicySnapshot,
    pub(in crate::stress) pressure: Pressure,
    pub(in crate::stress) controller: Revision,
    pub(in crate::stress) subject: Revision,
    pub(in crate::stress) selection: Selection,
    pub(in crate::stress) mode: String,
    pub(in crate::stress) system: SystemSnapshot,
    pub(in crate::stress) timing: Timing,
    pub(in crate::stress) schema: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::stress) struct ManifestSpec {
    pub(in crate::stress) build: BuildSnapshot,
    pub(in crate::stress) runner: ConfiguredLane,
    pub(in crate::stress) config: ManifestConfig,
    pub(in crate::stress) policy: PolicySnapshot,
    pub(in crate::stress) selection: Selection,
    pub(in crate::stress) controller_sha: String,
    pub(in crate::stress) mode: String,
    pub(in crate::stress) subject_sha: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::stress) struct ManifestConfig {
    pub(in crate::stress) nextest: String,
    pub(in crate::stress) pressure_schema: String,
    pub(in crate::stress) profile: String,
    pub(in crate::stress) workflow_job_timeout_minutes: u64,
}

impl ManifestConfig {
    #[must_use]
    pub(in crate::stress) fn new(
        nextest_profile: impl Into<String>,
        nextest: impl Into<String>,
        workflow_job_timeout_minutes: u64,
    ) -> Self {
        Self {
            workflow_job_timeout_minutes,
            profile: nextest_profile.into(),
            nextest: nextest.into(),
            pressure_schema: PRESSURE_SCHEMA.to_owned(),
        }
    }
}

/// Where the lane actually built, recorded rather than reconstructed.
///
/// A lane whose binaries went missing mid-run fails every remaining repeat in
/// milliseconds, and the one fact that names the cause — the directory those
/// binaries were in — used to appear nowhere in the evidence. It is an
/// observation, not a policy: the reporting machine has no such directory and
/// must not be asked to agree about one.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::stress) struct BuildSnapshot {
    pub(in crate::stress) target_dir: String,
}

impl BuildSnapshot {
    pub(in crate::stress) fn new(target_dir: &Path) -> Result<Self> {
        Ok(Self {
            target_dir: target_dir
                .to_str()
                .with_context(|| {
                    format!(
                        "stress build directory is not UTF-8: {}",
                        target_dir.display()
                    )
                })?
                .to_owned(),
        })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::stress) struct Revision {
    pub(in crate::stress) sha: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::stress) struct Selection {
    pub(in crate::stress) filter: String,
    pub(in crate::stress) test_threads: String,
    pub(in crate::stress) count: usize,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::stress) struct PolicySnapshot {
    pub(in crate::stress) raw_path_env: BTreeMap<String, String>,
    pub(in crate::stress) set_env: BTreeMap<String, String>,
    pub(in crate::stress) evidence: StressEvidenceConfig,
    pub(in crate::stress) features: Vec<String>,
    pub(in crate::stress) remove_env: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::stress) struct Timing {
    pub(in crate::stress) ended_at: Option<String>,
    pub(in crate::stress) exit_code: Option<i32>,
    pub(in crate::stress) started_at: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::stress) struct Pressure {
    pub(in crate::stress) sampler_healthy: Option<bool>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::stress) struct ExpectedProvenance {
    pub(in crate::stress) runner: ConfiguredLane,
    pub(in crate::stress) execute_result: ExecuteResult,
    pub(in crate::stress) config: ManifestConfig,
    pub(in crate::stress) policy: PolicySnapshot,
    pub(in crate::stress) controller_sha: String,
    pub(in crate::stress) filter: String,
    pub(in crate::stress) mode: String,
    pub(in crate::stress) subject_sha: String,
    pub(in crate::stress) test_threads: String,
    pub(in crate::stress) sampler_healthy: bool,
    pub(in crate::stress) count: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(in crate::stress) enum ExecuteResult {
    Success,
    Failure,
    Cancelled,
}

impl ExecuteResult {
    pub(in crate::stress) const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failure => "failure",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, derive_more::Display)]
#[display("{}={}, expected {}", field, actual, expected)]
pub(in crate::stress) struct ProvenanceMismatch {
    pub(in crate::stress) field: &'static str,
    pub(in crate::stress) actual: String,
    pub(in crate::stress) expected: String,
}

impl ProvenanceMismatch {
    fn new(field: &'static str, actual: impl Into<String>, expected: impl Into<String>) -> Self {
        Self {
            field,
            actual: actual.into(),
            expected: expected.into(),
        }
    }
}

impl Manifest {
    pub(in crate::stress) fn finalize(
        &mut self,
        exit_code: i32,
        sampler_healthy: bool,
    ) -> Result<()> {
        ensure!(
            self.timing.ended_at.is_none() && self.timing.exit_code.is_none(),
            "stress manifest is already finalized"
        );
        self.timing.ended_at = Some(format_timestamp(SystemTime::now())?);
        self.timing.exit_code = Some(exit_code);
        self.pressure.sampler_healthy = Some(sampler_healthy);
        Ok(())
    }

    pub(in crate::stress) fn read(path: &Path) -> Result<Self> {
        let file =
            File::open(path).with_context(|| format!("open stress manifest {}", path.display()))?;
        let mut contents = Vec::new();
        file.take(Consts::MANIFEST_READ_LIMIT)
            .read_to_end(&mut contents)
            .with_context(|| format!("read stress manifest {}", path.display()))?;
        ensure!(
            !contents.is_empty(),
            "stress manifest is empty: {}",
            path.display()
        );
        ensure!(
            contents.len() <= Consts::MAX_MANIFEST_BYTES,
            "stress manifest exceeds {} bytes: {}",
            Consts::MAX_MANIFEST_BYTES,
            path.display()
        );
        let manifest: Self = serde_json::from_slice(&contents)
            .with_context(|| format!("parse stress manifest {}", path.display()))?;
        ensure!(
            manifest.schema == Consts::MANIFEST_SCHEMA,
            "stress manifest schema is {}, expected {}",
            manifest.schema,
            Consts::MANIFEST_SCHEMA,
        );
        manifest.validate_invariants()?;
        Ok(manifest)
    }

    pub(in crate::stress) fn start(spec: ManifestSpec, system: SystemSnapshot) -> Result<Self> {
        let started_at = format_timestamp(SystemTime::now())?;
        let manifest = Self {
            system,
            schema: Consts::MANIFEST_SCHEMA,
            mode: spec.mode,
            build: spec.build,
            config: spec.config,
            controller: Revision {
                sha: spec.controller_sha,
            },
            subject: Revision {
                sha: spec.subject_sha,
            },
            runner: spec.runner,
            selection: spec.selection,
            policy: spec.policy,
            timing: Timing {
                started_at,
                ended_at: None,
                exit_code: None,
            },
            pressure: Pressure {
                sampler_healthy: None,
            },
        };
        manifest.validate_invariants()?;
        Ok(manifest)
    }

    fn validate_config(
        &self,
        expected: &ExpectedProvenance,
        mismatches: &mut Vec<ProvenanceMismatch>,
    ) {
        compare_string(
            "config.profile",
            &self.config.profile,
            &expected.config.profile,
            mismatches,
        );
        compare_string(
            "config.nextest",
            &self.config.nextest,
            &expected.config.nextest,
            mismatches,
        );
        compare_string(
            "config.pressure_schema",
            &self.config.pressure_schema,
            &expected.config.pressure_schema,
            mismatches,
        );
        if self.config.workflow_job_timeout_minutes != expected.config.workflow_job_timeout_minutes
        {
            mismatches.push(ProvenanceMismatch::new(
                "config.workflow_job_timeout_minutes",
                self.config.workflow_job_timeout_minutes.to_string(),
                expected.config.workflow_job_timeout_minutes.to_string(),
            ));
        }
    }

    fn validate_identity(
        &self,
        expected: &ExpectedProvenance,
        mismatches: &mut Vec<ProvenanceMismatch>,
    ) {
        if self.schema != Consts::MANIFEST_SCHEMA {
            mismatches.push(ProvenanceMismatch::new(
                "schema",
                self.schema.to_string(),
                Consts::MANIFEST_SCHEMA.to_string(),
            ));
        }
        compare_sha(
            "controller.sha",
            &self.controller.sha,
            &expected.controller_sha,
            mismatches,
        );
        compare_sha(
            "subject.sha",
            &self.subject.sha,
            &expected.subject_sha,
            mismatches,
        );
        if self.mode != expected.mode {
            mismatches.push(ProvenanceMismatch::new(
                "mode",
                quoted(&self.mode),
                quoted(&expected.mode),
            ));
        }
    }

    fn validate_invariants(&self) -> Result<()> {
        ensure!(
            valid_sha(&self.controller.sha),
            "manifest controller SHA is invalid"
        );
        ensure!(
            valid_sha(&self.subject.sha),
            "manifest subject SHA is invalid"
        );
        ensure!(!self.mode.trim().is_empty(), "manifest mode is empty");
        ensure!(
            Path::new(&self.build.target_dir).is_absolute(),
            "manifest build directory is not absolute: {}",
            self.build.target_dir
        );
        ensure!(
            !self.config.profile.trim().is_empty(),
            "manifest nextest profile is empty"
        );
        ensure!(
            !self.config.nextest.trim().is_empty(),
            "manifest nextest configuration path is empty"
        );
        ensure!(
            self.config.pressure_schema == PRESSURE_SCHEMA,
            "manifest pressure schema is invalid"
        );
        ensure!(
            self.config.workflow_job_timeout_minutes > 0,
            "manifest job timeout is zero"
        );
        ensure!(
            !self.selection.filter.trim().is_empty(),
            "manifest filter is empty"
        );
        ensure!(self.selection.count > 0, "manifest count is zero");
        ensure!(
            !self.selection.test_threads.trim().is_empty(),
            "manifest test-threads value is empty"
        );
        self.runner.validate()?;
        validate_policy(&self.policy)?;
        validate_timing_state(&self.timing, &self.pressure)?;
        Ok(())
    }

    fn validate_policy(
        &self,
        expected: &ExpectedProvenance,
        mismatches: &mut Vec<ProvenanceMismatch>,
    ) {
        compare_debug(
            "policy.features",
            &self.policy.features,
            &expected.policy.features,
            mismatches,
        );
        compare_debug(
            "policy.remove_env",
            &self.policy.remove_env,
            &expected.policy.remove_env,
            mismatches,
        );
        compare_debug(
            "policy.set_env",
            &self.policy.set_env,
            &expected.policy.set_env,
            mismatches,
        );
        compare_debug(
            "policy.raw_path_env",
            &self.policy.raw_path_env,
            &expected.policy.raw_path_env,
            mismatches,
        );
        compare_debug(
            "policy.evidence",
            &self.policy.evidence,
            &expected.policy.evidence,
            mismatches,
        );
    }

    fn validate_pressure(
        &self,
        expected: &ExpectedProvenance,
        mismatches: &mut Vec<ProvenanceMismatch>,
    ) {
        if self.pressure.sampler_healthy != Some(expected.sampler_healthy) {
            mismatches.push(ProvenanceMismatch::new(
                "pressure.sampler_healthy",
                optional_bool(self.pressure.sampler_healthy),
                expected.sampler_healthy.to_string(),
            ));
        }
    }

    #[must_use]
    pub(in crate::stress) fn validate_provenance(
        &self,
        expected: &ExpectedProvenance,
    ) -> Vec<ProvenanceMismatch> {
        let mut mismatches = Vec::new();
        self.validate_identity(expected, &mut mismatches);
        self.validate_config(expected, &mut mismatches);
        compare_debug("runner", &self.runner, &expected.runner, &mut mismatches);
        self.validate_selection(expected, &mut mismatches);
        self.validate_policy(expected, &mut mismatches);
        self.validate_timing(expected, &mut mismatches);
        self.validate_pressure(expected, &mut mismatches);
        mismatches
    }

    fn validate_selection(
        &self,
        expected: &ExpectedProvenance,
        mismatches: &mut Vec<ProvenanceMismatch>,
    ) {
        compare_string(
            "selection.filter",
            &self.selection.filter,
            &expected.filter,
            mismatches,
        );
        if self.selection.count != expected.count {
            mismatches.push(ProvenanceMismatch::new(
                "selection.count",
                self.selection.count.to_string(),
                expected.count.to_string(),
            ));
        }
        compare_string(
            "selection.test_threads",
            &self.selection.test_threads,
            &expected.test_threads,
            mismatches,
        );
    }

    /// A job's result covers every lane in it at once, so a red job means one lane failed, not
    /// necessarily this one; a lane that passed inside a failed job legitimately has a zero exit
    /// code.
    fn validate_timing(
        &self,
        expected: &ExpectedProvenance,
        mismatches: &mut Vec<ProvenanceMismatch>,
    ) {
        if self.timing.started_at.trim().is_empty() {
            mismatches.push(ProvenanceMismatch::new(
                "timing.started_at",
                quoted(&self.timing.started_at),
                "a non-empty timestamp",
            ));
        }
        match self.timing.ended_at.as_deref() {
            Some(value) if !value.trim().is_empty() => {}
            value => mismatches.push(ProvenanceMismatch::new(
                "timing.ended_at",
                optional_quoted(value),
                "a finalized non-empty timestamp",
            )),
        }
        let Some(exit_code) = self.timing.exit_code else {
            mismatches.push(ProvenanceMismatch::new(
                "timing.exit_code",
                "null",
                "a finalized integer exit code",
            ));
            return;
        };
        if matches!(expected.execute_result, ExecuteResult::Success) && exit_code != 0 {
            mismatches.push(ProvenanceMismatch::new(
                "timing.exit_code",
                format!(
                    "{exit_code} with execute result {}",
                    expected.execute_result.as_str()
                ),
                "zero for a successful execute job",
            ));
        }
    }

    pub(in crate::stress) fn write_atomic(&self, path: &Path) -> Result<()> {
        self.validate_invariants()?;
        let mut contents = serde_json::to_vec_pretty(self).context("serialize stress manifest")?;
        contents.push(b'\n');
        ensure!(
            contents.len() <= Consts::MAX_MANIFEST_BYTES,
            "stress manifest is {} bytes; maximum is {}",
            contents.len(),
            Consts::MAX_MANIFEST_BYTES,
        );
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).with_context(|| {
                format!("create stress manifest directory {}", parent.display())
            })?;
        }

        let temporary = temporary_path(path)?;
        let publication = write_and_publish(&temporary, path, &contents);
        if publication.is_err() {
            let _cleanup = fs::remove_file(&temporary);
        }
        publication
    }
}

fn validate_policy(policy: &PolicySnapshot) -> Result<()> {
    let mut features = BTreeSet::new();
    for feature in &policy.features {
        ensure!(
            !feature.trim().is_empty(),
            "manifest policy contains an empty feature"
        );
        ensure!(
            features.insert(feature.as_str()),
            "manifest policy contains duplicate feature {feature:?}"
        );
    }
    let mut removed = BTreeSet::new();
    for key in &policy.remove_env {
        validate_env_key(key)?;
        ensure!(
            removed.insert(key.as_str()),
            "manifest policy environment key {key:?} is removed more than once"
        );
    }
    let mut assigned = BTreeSet::new();
    for key in policy.set_env.keys() {
        validate_env_key(key)?;
        assigned.insert(key.as_str());
    }
    for (key, path) in &policy.raw_path_env {
        validate_env_key(key)?;
        ensure!(
            assigned.insert(key.as_str()),
            "manifest policy environment key {key:?} has conflicting assignments"
        );
        ensure!(
            safe_relative_path(path),
            "manifest policy raw path for {key:?} is not a safe relative path"
        );
    }
    for (field, value) in [
        (
            "envelope_schema",
            policy.evidence.envelope_schema.as_deref(),
        ),
        (
            "envelope_marker",
            policy.evidence.envelope_marker.as_deref(),
        ),
        (
            "envelope_text_field",
            policy.evidence.envelope_text_field.as_deref(),
        ),
        ("line_marker", policy.evidence.line_marker.as_deref()),
        (
            "primitive_marker",
            policy.evidence.primitive_marker.as_deref(),
        ),
        ("holder_marker", policy.evidence.holder_marker.as_deref()),
        ("wait_marker", policy.evidence.wait_marker.as_deref()),
    ] {
        validate_optional_text(field, value)?;
    }
    for marker in policy
        .evidence
        .envelope_suffix_markers
        .iter()
        .chain(&policy.evidence.direct_markers)
        .chain(&policy.evidence.source_excludes)
    {
        ensure!(
            !marker.trim().is_empty(),
            "manifest policy contains an empty evidence marker"
        );
    }
    Ok(())
}

fn validate_env_key(key: &str) -> Result<()> {
    ensure!(
        !key.trim().is_empty(),
        "manifest policy environment key is empty"
    );
    Ok(())
}

fn validate_optional_text(field: &str, value: Option<&str>) -> Result<()> {
    if let Some(value) = value {
        ensure!(
            !value.trim().is_empty(),
            "manifest policy field {field} is empty"
        );
    }
    Ok(())
}

fn validate_timing_state(timing: &Timing, pressure: &Pressure) -> Result<()> {
    ensure!(
        !timing.started_at.trim().is_empty(),
        "manifest start timestamp is empty"
    );
    match (
        timing.ended_at.as_deref(),
        timing.exit_code,
        pressure.sampler_healthy,
    ) {
        (None, None, None) => Ok(()),
        (Some(ended_at), Some(_), Some(_)) => {
            ensure!(
                !ended_at.trim().is_empty(),
                "manifest end timestamp is empty"
            );
            Ok(())
        }
        _ => bail!("manifest finalization fields are inconsistent"),
    }
}

fn safe_relative_path(value: &str) -> bool {
    !value.trim().is_empty()
        && Path::new(value)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn write_and_publish(temporary: &Path, destination: &Path, contents: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(temporary)
        .with_context(|| format!("create temporary stress manifest {}", temporary.display()))?;
    file.write_all(contents)
        .with_context(|| format!("write temporary stress manifest {}", temporary.display()))?;
    file.sync_all()
        .with_context(|| format!("sync temporary stress manifest {}", temporary.display()))?;
    drop(file);
    fs::rename(temporary, destination).with_context(|| {
        format!(
            "publish stress manifest {} as {}",
            temporary.display(),
            destination.display()
        )
    })
}

fn temporary_path(path: &Path) -> Result<PathBuf> {
    let file_name = path
        .file_name()
        .with_context(|| format!("stress manifest path has no file name: {}", path.display()))?;
    let mut temporary_name = file_name.to_os_string();
    temporary_name.push(format!(".{}.tmp", std::process::id()));
    Ok(path.with_file_name(temporary_name))
}

fn compare_sha(
    field: &'static str,
    actual: &str,
    expected: &str,
    mismatches: &mut Vec<ProvenanceMismatch>,
) {
    if !actual.eq_ignore_ascii_case(expected) {
        mismatches.push(ProvenanceMismatch::new(
            field,
            quoted(actual),
            quoted(expected),
        ));
    }
}

fn compare_string(
    field: &'static str,
    actual: &str,
    expected: &str,
    mismatches: &mut Vec<ProvenanceMismatch>,
) {
    if actual != expected {
        mismatches.push(ProvenanceMismatch::new(
            field,
            quoted(actual),
            quoted(expected),
        ));
    }
}

fn compare_debug<T: fmt::Debug + PartialEq>(
    field: &'static str,
    actual: &T,
    expected: &T,
    mismatches: &mut Vec<ProvenanceMismatch>,
) {
    if actual != expected {
        mismatches.push(ProvenanceMismatch::new(
            field,
            format!("{actual:?}"),
            format!("{expected:?}"),
        ));
    }
}

fn valid_sha(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn quoted(value: &str) -> String {
    format!("{value:?}")
}

fn optional_quoted(value: Option<&str>) -> String {
    value.map_or_else(|| "null".to_owned(), quoted)
}

fn optional_bool(value: Option<bool>) -> String {
    value.map_or_else(|| "null".to_owned(), |value| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stress::system::{CgroupScope, CgroupV2, CpuSet, Limits};

    const CONTROLLER_SHA: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
    const SUBJECT_SHA: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn system() -> SystemSnapshot {
        SystemSnapshot {
            kernel: "Linux 6.12-test #1 x86_64".to_owned(),
            cgroup_v2: CgroupV2 {
                scope: CgroupScope::CurrentProcessCgroup,
                path: Some(PathBuf::from("/sys/fs/cgroup/job.scope")),
            },
            cpuset: CpuSet {
                cgroup_effective: Some("0-3".to_owned()),
                proc_allowed: Some("0-3".to_owned()),
            },
            limits: Limits {
                cgroup_cpu_max: Some("max 100000".to_owned()),
                cgroup_memory_max: None,
                cgroup_pids_max: Some("512".to_owned()),
                ulimit_open_files: Some("1024".to_owned()),
                ulimit_processes: Some("4096".to_owned()),
            },
        }
    }

    fn policy() -> PolicySnapshot {
        PolicySnapshot {
            features: vec!["snapshot-clock".to_owned()],
            remove_env: vec!["LEGACY_SETTING".to_owned()],
            set_env: BTreeMap::from([
                ("OUTPUT_STYLE".to_owned(), "compact".to_owned()),
                ("TRACE_LEVEL".to_owned(), "verbose".to_owned()),
            ]),
            raw_path_env: BTreeMap::from([(
                "CAPTURE_PATH".to_owned(),
                "captures/events.jsonl".to_owned(),
            )]),
            evidence: StressEvidenceConfig {
                envelope_schema: Some("thread-snapshot-v2".to_owned()),
                envelope_text_field: Some("wait_graph".to_owned()),
                line_marker: Some("blocking-observation-v2".to_owned()),
                ..StressEvidenceConfig::default()
            },
        }
    }

    fn runner() -> ConfiguredLane {
        ConfiguredLane {
            lane: "workspace".to_owned(),
            backend: "http".to_owned(),
            program: "cargo".to_owned(),
            prefix_args: vec!["nextest".to_owned(), "run".to_owned()],
            suffix_args: vec!["--locked".to_owned()],
            feature_arg: "--features".to_owned(),
            features: vec!["snapshot-clock".to_owned()],
            env: BTreeMap::new(),
        }
    }

    fn spec() -> ManifestSpec {
        ManifestSpec {
            mode: "baseline".to_owned(),
            build: BuildSnapshot::new(Path::new("/stress/target-stress")).expect("build"),
            config: ManifestConfig::new("repeated", "controller/settings/runner.toml", 90),
            controller_sha: CONTROLLER_SHA.to_owned(),
            subject_sha: SUBJECT_SHA.to_owned(),
            runner: runner(),
            selection: Selection {
                filter: "all()".to_owned(),
                count: 50,
                test_threads: "num-cpus".to_owned(),
            },
            policy: policy(),
        }
    }

    fn expected() -> ExpectedProvenance {
        ExpectedProvenance {
            controller_sha: CONTROLLER_SHA.to_ascii_lowercase(),
            subject_sha: SUBJECT_SHA.to_ascii_uppercase(),
            filter: "all()".to_owned(),
            count: 50,
            test_threads: "num-cpus".to_owned(),
            mode: "baseline".to_owned(),
            config: ManifestConfig::new("repeated", "controller/settings/runner.toml", 90),
            runner: runner(),
            policy: policy(),
            execute_result: ExecuteResult::Success,
            sampler_healthy: true,
        }
    }

    #[test]
    fn finalized_manifest_round_trips_through_atomic_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("raw/manifest.json");
        let mut manifest = Manifest::start(spec(), system()).expect("start manifest");
        manifest.finalize(0, true).expect("finalize manifest");

        manifest.write_atomic(&path).expect("write manifest");
        let parsed = Manifest::read(&path).expect("read manifest");

        assert_eq!(parsed, manifest);
        assert_eq!(parsed.validate_provenance(&expected()), Vec::new());
        let json: serde_json::Value =
            serde_json::from_slice(&fs::read(path).expect("read JSON")).expect("parse JSON");
        assert_eq!(json["schema"], Consts::MANIFEST_SCHEMA);
        assert_eq!(json["selection"]["count"], 50);
        assert_eq!(json["pressure"]["sampler_healthy"], true);
    }

    #[test]
    fn provenance_validation_accumulates_actionable_mismatches() {
        let manifest = Manifest::start(spec(), system()).expect("start manifest");
        let mut expected = expected();
        expected.subject_sha = "cccccccccccccccccccccccccccccccccccccccc".to_owned();
        expected.filter = "package(foo)".to_owned();
        expected.count = 100;
        expected.test_threads = "single".to_owned();
        expected.mode = "instrumented".to_owned();
        expected.config = ManifestConfig::new("alternate", "other/runner.toml", 60);
        expected.config.pressure_schema = "pressure-vNext".to_owned();
        expected.runner.features = vec!["alternate".to_owned()];
        expected.policy = PolicySnapshot {
            features: vec!["blocking-census".to_owned()],
            remove_env: vec!["DEPRECATED_SETTING".to_owned()],
            set_env: BTreeMap::from([("TRACE_LEVEL".to_owned(), "quiet".to_owned())]),
            raw_path_env: BTreeMap::from([(
                "EVENT_PATH".to_owned(),
                "observations/events.jsonl".to_owned(),
            )]),
            evidence: StressEvidenceConfig {
                envelope_schema: Some("thread-snapshot-v3".to_owned()),
                envelope_text_field: Some("edges".to_owned()),
                ..StressEvidenceConfig::default()
            },
        };

        let mismatches = manifest.validate_provenance(&expected);
        let fields = mismatches
            .iter()
            .map(|mismatch| mismatch.field)
            .collect::<Vec<_>>();

        assert!(!fields.contains(&"controller.sha"));
        for field in [
            "subject.sha",
            "mode",
            "config.profile",
            "config.nextest",
            "config.pressure_schema",
            "config.workflow_job_timeout_minutes",
            "runner",
            "policy.features",
            "policy.remove_env",
            "policy.set_env",
            "policy.raw_path_env",
            "policy.evidence",
            "selection.filter",
            "selection.count",
            "selection.test_threads",
            "timing.ended_at",
            "timing.exit_code",
            "pressure.sampler_healthy",
        ] {
            assert!(fields.contains(&field), "missing mismatch for {field}");
        }
        assert!(
            mismatches
                .iter()
                .all(|mismatch| mismatch.to_string().contains("expected"))
        );
    }

    #[test]
    fn execute_result_must_agree_with_final_exit_code() {
        let mut manifest = Manifest::start(spec(), system()).expect("start manifest");
        manifest.finalize(17, true).expect("finalize manifest");

        let success_mismatches = manifest.validate_provenance(&expected());
        assert!(
            success_mismatches
                .iter()
                .any(|mismatch| mismatch.field == "timing.exit_code")
        );

        for execute_result in [ExecuteResult::Failure, ExecuteResult::Cancelled] {
            let mut expected = expected();
            expected.execute_result = execute_result;
            assert_eq!(manifest.validate_provenance(&expected), Vec::new());
        }
    }

    /// A run has several lanes in one job, so a red job means one of
    /// them failed rather than this one. Marking a lane that passed as
    /// untrustworthy would drop exactly the clean half of a comparison.
    #[test]
    fn a_lane_that_passed_inside_a_failed_job_keeps_its_provenance() {
        let mut manifest = Manifest::start(spec(), system()).expect("start manifest");
        manifest.finalize(0, true).expect("finalize manifest");

        for execute_result in [ExecuteResult::Failure, ExecuteResult::Cancelled] {
            let mut expected = expected();
            expected.execute_result = execute_result;
            assert_eq!(manifest.validate_provenance(&expected), Vec::new());
        }
    }

    #[test]
    fn bounded_reader_rejects_an_oversized_manifest() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("manifest.json");
        fs::write(&path, vec![b' '; Consts::MAX_MANIFEST_BYTES + 1])
            .expect("write oversized fixture");

        let error = Manifest::read(&path).expect_err("oversized manifest must fail");

        assert!(error.to_string().contains("exceeds"), "{error:#}");
    }

    #[test]
    fn reader_rejects_a_tampered_empty_nextest_path() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("manifest.json");
        let mut manifest = Manifest::start(spec(), system()).expect("start manifest");
        manifest.finalize(0, true).expect("finalize manifest");
        let mut json = serde_json::to_value(manifest).expect("serialize manifest");
        json["config"]["nextest"] = serde_json::Value::String(String::new());
        fs::write(&path, serde_json::to_vec(&json).expect("encode manifest"))
            .expect("write manifest");

        let error = Manifest::read(&path).expect_err("empty nextest path must fail");

        assert!(error.to_string().contains("nextest configuration"));
    }

    #[test]
    fn reader_rejects_conflicting_policy_assignments() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("manifest.json");
        let manifest = Manifest::start(spec(), system()).expect("start manifest");
        let mut json = serde_json::to_value(manifest).expect("serialize manifest");
        json["policy"]["raw_path_env"]["TRACE_LEVEL"] =
            serde_json::Value::String("captures/trace.log".to_owned());
        fs::write(&path, serde_json::to_vec(&json).expect("encode manifest"))
            .expect("write manifest");

        let error = Manifest::read(&path).expect_err("conflicting policy must be rejected");

        assert!(error.to_string().contains("conflicting assignments"));
    }

    #[test]
    fn reader_rejects_unsafe_raw_policy_path() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("manifest.json");
        let manifest = Manifest::start(spec(), system()).expect("start manifest");
        let mut json = serde_json::to_value(manifest).expect("serialize manifest");
        json["policy"]["raw_path_env"]["CAPTURE_PATH"] =
            serde_json::Value::String("../outside.jsonl".to_owned());
        fs::write(&path, serde_json::to_vec(&json).expect("encode manifest"))
            .expect("write manifest");

        let error = Manifest::read(&path).expect_err("unsafe raw path must be rejected");

        assert!(error.to_string().contains("safe relative path"));
    }

    #[test]
    fn bounded_writer_does_not_publish_an_oversized_manifest() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("manifest.json");
        let mut spec = spec();
        spec.selection.filter = "x".repeat(Consts::MAX_MANIFEST_BYTES);
        let manifest = Manifest::start(spec, system()).expect("start manifest");

        let error = manifest
            .write_atomic(&path)
            .expect_err("oversized manifest must fail");

        assert!(error.to_string().contains("maximum"), "{error:#}");
        assert!(!path.exists());
    }

    #[test]
    fn rejects_an_unrecognized_schema_after_bounded_parse() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("manifest.json");
        let manifest = Manifest::start(spec(), system()).expect("start manifest");
        let mut json = serde_json::to_value(manifest).expect("serialize manifest");
        json["schema"] = serde_json::json!(Consts::MANIFEST_SCHEMA + 1);
        fs::write(&path, serde_json::to_vec(&json).expect("encode fixture"))
            .expect("write fixture");

        let error = Manifest::read(&path).expect_err("unknown schema must fail");

        assert!(error.to_string().contains("schema"), "{error:#}");
    }
}

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, clap::ValueEnum, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum BenchMode {
    #[default]
    Timing,
    Allocation,
}

impl std::fmt::Display for BenchMode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl BenchMode {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Timing => "timing",
            Self::Allocation => "allocation",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, clap::ValueEnum, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum AllocatorPolicy {
    #[default]
    System,
    AllocDisabler,
    Counting,
}

impl std::fmt::Display for AllocatorPolicy {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl AllocatorPolicy {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::AllocDisabler => "alloc-disabler",
            Self::Counting => "counting",
        }
    }

    fn inner_backend(self) -> &'static str {
        match self {
            Self::System | Self::AllocDisabler | Self::Counting => "system",
        }
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct AllocationReport {
    entries: Vec<AllocationEntry>,
}

#[derive(Debug, Deserialize)]
struct AllocationEntry {
    identity: String,
    results: BTreeMap<String, AllocationBackend>,
}

#[derive(Debug, Deserialize)]
struct AllocationBackend {
    metrics: BTreeMap<String, AllocationMetric>,
    raw_artifacts: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct AllocationMetric {
    value: u64,
}

#[derive(Debug, Serialize)]
pub(super) struct CanonicalReport<'a> {
    schema_version: u32,
    target: &'a str,
    mode: BenchMode,
    allocator_policy: AllocatorPolicy,
    inner_backend: &'static str,
    required_features: &'a [String],
    collection: CollectionScope,
    cases: Vec<CanonicalCase>,
}

#[derive(Debug, Serialize)]
struct CollectionScope {
    boundaries: &'static str,
    threads: &'static str,
    overhead: &'static str,
    excludes: &'static str,
}

#[derive(Debug, Serialize)]
struct CanonicalCase {
    identity: String,
    allocations: u64,
    allocated_bytes: u64,
    raw_artifacts: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    guard: Option<GuardObservation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    heap: Option<HeapObservation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    worker: Option<WorkerObservation>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    observations: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(super) struct Observation {
    guard: Option<GuardObservation>,
    heap: Option<HeapObservation>,
    worker: Option<WorkerObservation>,
    other: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct GuardObservation {
    status: String,
    policy: String,
    inner_backend: String,
    scope: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct WorkerObservation {
    joined: usize,
    escaped: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct HeapObservation {
    held_bytes: usize,
    peak_bytes: usize,
    baseline_bytes: usize,
    window: String,
    policy: String,
    inner_backend: String,
}

#[derive(Deserialize)]
struct ObservationRecord {
    identity: String,
    observation: serde_json::Value,
}

pub(super) fn read_observations(
    path: &Path,
    policy: AllocatorPolicy,
) -> Result<BTreeMap<String, Observation>> {
    if policy == AllocatorPolicy::System && !path.exists() {
        return Ok(BTreeMap::new());
    }
    let bytes =
        fs::read(path).with_context(|| format!("read owner observations {}", path.display()))?;
    let mut observations = BTreeMap::new();
    for line in bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        let record: ObservationRecord = serde_json::from_slice(line)
            .with_context(|| format!("parse owner observation in {}", path.display()))?;
        let observation: &mut Observation = observations.entry(record.identity).or_default();
        if let Some(guard) = record.observation.get("guard") {
            ensure!(observation.guard.is_none(), "duplicate guard observation");
            observation.guard = Some(serde_json::from_value(guard.clone())?);
        }
        if let Some(heap) = record.observation.get("heap") {
            ensure!(observation.heap.is_none(), "duplicate heap observation");
            observation.heap = Some(serde_json::from_value(heap.clone())?);
        }
        if let Some(worker) = record.observation.get("worker") {
            ensure!(observation.worker.is_none(), "duplicate worker observation");
            observation.worker = Some(serde_json::from_value(worker.clone())?);
        }
        for (name, value) in record.observation.as_object().into_iter().flatten() {
            if !matches!(name.as_str(), "guard" | "heap" | "worker") {
                observation.other.insert(name.clone(), value.clone());
            }
        }
    }
    Ok(observations)
}

impl CanonicalReport<'_> {
    pub(super) fn write(&self, path: &Path) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(self)?;
        fs::write(path, bytes).with_context(|| format!("write {}", path.display()))
    }
}

pub(super) fn read_allocation(path: &Path) -> Result<AllocationReport> {
    let bytes =
        fs::read(path).with_context(|| format!("read allocation report {}", path.display()))?;
    let report: AllocationReport = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse allocation report {}", path.display()))?;
    ensure!(!report.entries.is_empty(), "allocation report has no cases");
    let mut seen = BTreeSet::new();
    for entry in &report.entries {
        ensure!(
            seen.insert(&entry.identity),
            "duplicate allocation case {:?}",
            entry.identity
        );
        let backend = entry.results.get("alloc_tracker").with_context(|| {
            format!(
                "allocation case {:?} has no alloc_tracker result",
                entry.identity
            )
        })?;
        for metric in ["Allocations", "Allocated bytes"] {
            ensure!(
                backend.metrics.contains_key(metric),
                "allocation case {:?} has no {metric} metric",
                entry.identity
            );
        }
    }
    Ok(report)
}

impl AllocationReport {
    pub(super) fn ids(&self) -> BTreeSet<String> {
        self.entries
            .iter()
            .map(|entry| entry.identity.clone())
            .collect()
    }

    pub(super) fn canonical<'a>(
        &self,
        target: &'a str,
        features: &'a [String],
        policy: AllocatorPolicy,
        observations: &BTreeMap<String, Observation>,
    ) -> Result<CanonicalReport<'a>> {
        if policy != AllocatorPolicy::System {
            ensure!(
                observations.len() == self.entries.len(),
                "owner observation inventory does not match allocation cases"
            );
        }
        let cases = self
            .entries
            .iter()
            .map(|entry| {
                let backend = &entry.results["alloc_tracker"];
                let observation = observations.get(&entry.identity);
                if let Some(worker) = observation.and_then(|value| value.worker.as_ref()) {
                    ensure!(
                        worker.joined > 0 && worker.escaped == 0,
                        "allocation case {:?} has escaped or unjoined workers",
                        entry.identity
                    );
                }
                match policy {
                    AllocatorPolicy::System => {
                        if let Some(observation) = observation {
                            ensure!(
                                observation.guard.is_none() && observation.heap.is_none(),
                                "allocation case {:?} has a guard/heap policy observation but the request selects System",
                                entry.identity
                            );
                            if let Some(timing) = observation.other.get("timing") {
                                ensure!(
                                    timing["policy"] == "system" && timing["inner_backend"] == "system",
                                    "allocation case {:?} has a non-System allocator observation",
                                    entry.identity
                                );
                            }
                        }
                    }
                    AllocatorPolicy::AllocDisabler => {
                        let guard = observation.and_then(|value| value.guard.as_ref());
                        ensure!(
                            guard.is_some_and(|guard| {
                                guard.status == "passed"
                                    && guard.policy == "alloc_disabler"
                                    && guard.inner_backend == "system"
                            }),
                            "allocation case {:?} has no passing System alloc_disabler guard observation",
                            entry.identity
                        );
                    }
                    AllocatorPolicy::Counting => {
                        let heap = observation.and_then(|value| value.heap.as_ref());
                        ensure!(
                            heap.is_some_and(|heap| {
                                heap.policy == "counting" && heap.inner_backend == "system"
                            }),
                            "allocation case {:?} has no System counting heap observation",
                            entry.identity
                        );
                    }
                }
                Ok(CanonicalCase {
                    identity: entry.identity.clone(),
                    allocations: backend.metrics["Allocations"].value,
                    allocated_bytes: backend.metrics["Allocated bytes"].value,
                    raw_artifacts: backend.raw_artifacts.clone(),
                    guard: observation.and_then(|value| value.guard.clone()),
                    heap: observation.and_then(|value| value.heap.clone()),
                    worker: observation.and_then(|value| value.worker.clone()),
                    observations: observation
                        .map(|value| value.other.clone())
                        .unwrap_or_default(),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(CanonicalReport {
            schema_version: 1,
            target,
            mode: BenchMode::Allocation,
            allocator_policy: policy,
            inner_backend: policy.inner_backend(),
            required_features: features,
            collection: CollectionScope {
                boundaries: "one completed native probe action per case",
                threads: "process-wide, including joined worker allocations inside the probe window",
                overhead: "the selected global allocator wrapper remains installed; disabled tracking still checks its flag",
                excludes: "live/peak heap, forbidden frees, domain budgets, resident memory and GPU memory",
            },
            cases,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(contents: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("allocation.json");
        fs::write(&path, contents).unwrap();
        (directory, path)
    }

    #[test]
    fn allocation_results_attach_to_their_exact_case() {
        let (_directory, path) = report(
            r#"{"entries":[{
                "identity":"group/case",
                "results":{"alloc_tracker":{
                    "metrics":{
                        "Allocations":{"value":2},
                        "Allocated bytes":{"value":128}
                    },
                    "raw_artifacts":["runs/1/allocations/0.json"]
                }}
            }]}"#,
        );
        let report = read_allocation(&path).unwrap();
        assert_eq!(report.ids(), BTreeSet::from(["group/case".to_owned()]));
        let observations_path = path.with_file_name("observations.jsonl");
        fs::write(
            &observations_path,
            r#"{"identity":"group/case","observation":{"heap":{"held_bytes":0,"peak_bytes":0,"baseline_bytes":64,"window":"reset; action; read","policy":"counting","inner_backend":"system"}}}"#,
        )
        .unwrap();
        let observations =
            read_observations(&observations_path, AllocatorPolicy::Counting).unwrap();
        let canonical = report
            .canonical(
                "package:bench:",
                &[],
                AllocatorPolicy::Counting,
                &observations,
            )
            .unwrap();
        let path = path.with_file_name("canonical.json");
        canonical.write(&path).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(value["mode"], "allocation");
        assert_eq!(value["allocator_policy"], "counting");
        assert_eq!(value["inner_backend"], "system");
        assert_eq!(value["cases"][0]["identity"], "group/case");
        assert_eq!(value["cases"][0]["allocations"], 2);
        assert_eq!(value["cases"][0]["allocated_bytes"], 128);
        assert_eq!(value["cases"][0]["heap"]["baseline_bytes"], 64);
        assert!(
            value["collection"]["threads"]
                .as_str()
                .unwrap()
                .contains("joined worker")
        );
    }

    #[test]
    fn policy_observations_are_mandatory_and_case_scoped() {
        let (_directory, path) = report(
            r#"{"entries":[{
                "identity":"group/case",
                "results":{"alloc_tracker":{
                    "metrics":{
                        "Allocations":{"value":0},
                        "Allocated bytes":{"value":0}
                    },
                    "raw_artifacts":[]
                }}
            }]}"#,
        );
        let report = read_allocation(&path).unwrap();
        let error = report
            .canonical(
                "package:bench:",
                &[],
                AllocatorPolicy::AllocDisabler,
                &BTreeMap::new(),
            )
            .unwrap_err();
        assert!(error.to_string().contains("observation inventory"));
        let observations = BTreeMap::from([(
            "group/case".to_owned(),
            Observation {
                worker: Some(WorkerObservation {
                    joined: 0,
                    escaped: 1,
                }),
                ..Observation::default()
            },
        )]);
        let error = report
            .canonical(
                "package:bench:",
                &[],
                AllocatorPolicy::System,
                &observations,
            )
            .unwrap_err();
        assert!(error.to_string().contains("escaped or unjoined workers"));
    }

    #[test]
    fn incomplete_allocation_results_are_rejected() {
        for (contents, error) in [
            (r#"{"entries":[]}"#, "no cases"),
            (
                r#"{"entries":[{"identity":"a","results":{}}]}"#,
                "no alloc_tracker result",
            ),
            (
                r#"{"entries":[{"identity":"a","results":{"alloc_tracker":{"metrics":{},"raw_artifacts":[]}}}]}"#,
                "no Allocations metric",
            ),
            (
                r#"{"entries":[
                    {"identity":"a","results":{"alloc_tracker":{"metrics":{"Allocations":{"value":0},"Allocated bytes":{"value":0}},"raw_artifacts":[]}}},
                    {"identity":"a","results":{"alloc_tracker":{"metrics":{"Allocations":{"value":0},"Allocated bytes":{"value":0}},"raw_artifacts":[]}}}
                ]}"#,
                "duplicate allocation case",
            ),
        ] {
            let (_directory, path) = report(contents);
            let actual = read_allocation(&path).unwrap_err();
            assert!(format!("{actual:#}").contains(error), "{actual:#}");
        }
    }
}

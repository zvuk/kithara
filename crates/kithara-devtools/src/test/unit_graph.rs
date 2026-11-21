use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct UnitGraph {
    version: u64,
    units: Vec<Unit>,
    roots: Vec<usize>,
}

#[derive(Debug, Deserialize, Serialize)]
struct Unit {
    pkg_id: String,
    target: Value,
    profile: Value,
    platform: Value,
    mode: CompileMode,
    features: Vec<String>,
    dependencies: Vec<Dependency>,
    #[serde(flatten)]
    properties: BTreeMap<String, Value>,
}

#[derive(Debug, Deserialize, Serialize)]
struct Dependency {
    index: usize,
    #[serde(flatten)]
    properties: BTreeMap<String, Value>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum CompileMode {
    Build,
    Check,
    Test,
    Doc,
    Doctest,
    RunCustomBuild,
    DocScrape,
}

impl UnitGraph {
    pub(super) fn has_selected_roots(&self, packages: &BTreeSet<String>) -> bool {
        self.roots
            .iter()
            .any(|&root| packages.contains(&self.units[root].pkg_id))
    }

    pub(super) fn closure_packages(&self, packages: &BTreeSet<String>) -> BTreeSet<String> {
        let mut pending: Vec<_> = self
            .roots
            .iter()
            .copied()
            .filter(|&root| packages.contains(&self.units[root].pkg_id))
            .collect();
        let mut visited = BTreeSet::new();
        let mut reached = BTreeSet::new();
        while let Some(index) = pending.pop() {
            if visited.insert(index) {
                let unit = &self.units[index];
                reached.insert(unit.pkg_id.clone());
                pending.extend(unit.dependencies.iter().map(|dependency| dependency.index));
            }
        }
        reached
    }

    pub(super) fn parse(bytes: &[u8]) -> Result<Self> {
        let graph: Self = serde_json::from_slice(bytes).context("parse Cargo unit graph")?;
        if graph.version != 1 {
            bail!("unsupported Cargo unit graph version {}", graph.version);
        }
        let mut roots = BTreeSet::new();
        for &root in &graph.roots {
            if root >= graph.units.len() || !roots.insert(root) {
                bail!("Cargo unit graph has an invalid or duplicate root {root}");
            }
        }
        for unit in &graph.units {
            if unit.pkg_id.is_empty() || !unit.profile.is_object() {
                bail!("Cargo unit graph has an invalid package or profile");
            }
            for field in ["name", "src_path", "edition"] {
                if !unit.target[field]
                    .as_str()
                    .is_some_and(|value| !value.is_empty())
                {
                    bail!("Cargo unit graph target has no {field}");
                }
            }
            if !unit.platform.is_null() && !unit.platform.is_string() {
                bail!("Cargo unit graph has an unsupported platform");
            }
            let kinds = unit.target["kind"]
                .as_array()
                .context("Cargo unit graph target has no kinds")?;
            if kinds.is_empty() {
                bail!("Cargo unit graph target has no kind");
            }
            for kind in kinds {
                if !matches!(
                    kind.as_str(),
                    Some(
                        "lib"
                            | "rlib"
                            | "dylib"
                            | "cdylib"
                            | "staticlib"
                            | "proc-macro"
                            | "bin"
                            | "test"
                            | "bench"
                            | "example"
                            | "custom-build"
                    )
                ) {
                    bail!("unsupported Cargo unit graph target kind {kind}");
                }
            }
            if unit
                .dependencies
                .iter()
                .any(|d| d.index >= graph.units.len())
            {
                bail!("Cargo unit graph has a dangling dependency index");
            }
        }
        graph.digests()?;
        Ok(graph)
    }

    pub(super) fn selected_roots(
        &self,
        packages: &BTreeSet<String>,
    ) -> Result<BTreeMap<String, usize>> {
        let digests = self.digests()?;
        let mut selected = BTreeMap::new();
        for &root in &self.roots {
            if packages.contains(&self.units[root].pkg_id) {
                *selected.entry(digests[root].clone()).or_default() += 1;
            }
        }
        if selected.is_empty() {
            bail!("Cargo unit graph matched no active selected roots");
        }
        Ok(selected)
    }

    pub(super) fn all_roots(&self) -> Result<BTreeMap<String, usize>> {
        self.selected_roots(&self.units.iter().map(|u| u.pkg_id.clone()).collect())
    }

    pub(super) fn verify_artifacts(&self, output: &[u8]) -> Result<()> {
        let expected = self
            .units
            .iter()
            .filter(|unit| unit.mode != CompileMode::RunCustomBuild)
            .map(|unit| {
                artifact_key(
                    &unit.pkg_id,
                    &unit.target,
                    &unit.profile,
                    &unit.features,
                    unit.mode == CompileMode::Test,
                )
            })
            .collect::<BTreeSet<_>>();
        let mut actual = BTreeSet::new();
        for line in output
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
        {
            let message: Value =
                serde_json::from_slice(line).context("parse native Cargo artifact")?;
            if message["reason"] != "compiler-artifact" {
                continue;
            }
            let package = message["package_id"]
                .as_str()
                .context("native artifact has no package ID")?;
            let features: Vec<String> = serde_json::from_value(message["features"].clone())?;
            let test = message["profile"]["test"]
                .as_bool()
                .context("native artifact has no test mode")?;
            actual.insert(artifact_key(
                package,
                &message["target"],
                &message["profile"],
                &features,
                test,
            ));
        }
        if actual.is_empty() || actual != expected {
            bail!(
                "native Cargo artifacts disagree with the admitted graph: {} missing, {} unexpected",
                expected.difference(&actual).count(),
                actual.difference(&expected).count()
            );
        }
        Ok(())
    }

    fn digests(&self) -> Result<Vec<String>> {
        let mut digests = vec![None; self.units.len()];
        let mut pending = BTreeSet::new();
        for index in 0..self.units.len() {
            self.digest(index, &mut digests, &mut pending)?;
        }
        digests
            .into_iter()
            .map(|digest| digest.context("Cargo unit digest was not computed"))
            .collect()
    }

    fn digest(
        &self,
        index: usize,
        digests: &mut [Option<String>],
        pending: &mut BTreeSet<usize>,
    ) -> Result<String> {
        if let Some(digest) = &digests[index] {
            return Ok(digest.clone());
        }
        if !pending.insert(index) {
            bail!("Cargo unit graph contains a dependency cycle");
        }
        let unit = &self.units[index];
        let mut dependencies = Vec::new();
        for dependency in &unit.dependencies {
            let mut edge = dependency.properties.clone();
            edge.insert(
                "unit-digest".to_owned(),
                Value::String(self.digest(dependency.index, digests, pending)?),
            );
            dependencies.push(serde_json::to_string(&edge)?);
        }
        dependencies.sort();
        let mut value = serde_json::to_value(unit)?;
        value["dependencies"] = json!(dependencies);
        let mut features = unit.features.clone();
        features.sort();
        value["features"] = json!(features);
        let digest = hex::encode(Sha256::digest(canonical(&value).to_string()));
        pending.remove(&index);
        digests[index] = Some(digest.clone());
        Ok(digest)
    }
}

fn canonical(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let sorted: BTreeMap<_, _> = map.iter().map(|(k, v)| (k, canonical(v))).collect();
            json!(sorted)
        }
        Value::Array(items) => Value::Array(items.iter().map(canonical).collect()),
        other => other.clone(),
    }
}

fn artifact_key(
    package: &str,
    target: &Value,
    profile: &Value,
    features: &[String],
    test: bool,
) -> String {
    let features: BTreeSet<_> = features.iter().collect();
    canonical(&json!({
        "package": package,
        "target": {
            "name": target["name"], "kind": target["kind"],
            "crate_types": target["crate_types"], "src_path": target["src_path"],
            "edition": target["edition"],
        },
        "profile": {
            "opt_level": profile["opt_level"], "debuginfo": profile["debuginfo"],
            "debug_assertions": profile["debug_assertions"],
            "overflow_checks": profile["overflow_checks"], "test": test,
        },
        "features": features,
    }))
    .to_string()
}

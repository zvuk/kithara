use std::{fs, path::Path, time::Duration};

use anyhow::{Context, Result, bail};
use cargo_metadata::{Metadata, Package, TargetKind};
use serde::Serialize;

use super::{
    graph::EvidenceGraph,
    trace::{self, TraceState, TraceSummary},
};
use crate::common::{
    process::{ProcessRequest, run_process_with_env},
    project::{ArchitectureConfig, RuntimeScenarioConfig},
};

const TRACE_ENV: &str = "ARCHITECTURE_TRACE_PATH";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ScenarioState {
    Complete,
    Empty,
    Truncated,
    Failed,
    TimedOut,
    Manual,
}

impl ScenarioState {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Empty => "empty",
            Self::Truncated => "truncated",
            Self::Failed => "failed",
            Self::TimedOut => "timed_out",
            Self::Manual => "manual",
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct ScenarioSummary {
    pub(crate) name: String,
    pub(crate) state: ScenarioState,
    pub(crate) exit_code: Option<i32>,
    pub(crate) trace: TraceSummary,
    pub(crate) stdout: Option<String>,
    pub(crate) stderr: Option<String>,
}

#[derive(Debug, Default, Serialize)]
pub(crate) struct RuntimeSummary {
    pub(crate) scenarios: Vec<ScenarioSummary>,
}

impl RuntimeSummary {
    pub(crate) fn is_incomplete(&self) -> bool {
        self.scenarios.iter().any(|scenario| {
            matches!(
                scenario.state,
                ScenarioState::Empty | ScenarioState::Failed | ScenarioState::TimedOut
            )
        })
    }

    pub(crate) fn is_truncated(&self) -> bool {
        self.scenarios
            .iter()
            .any(|scenario| scenario.state == ScenarioState::Truncated)
    }

    pub(crate) fn has_runtime(&self) -> bool {
        !self.scenarios.is_empty()
    }
}

pub(crate) struct RunRequest<'a> {
    pub(crate) config: &'a ArchitectureConfig,
    pub(crate) metadata: &'a Metadata,
    pub(crate) root: &'a Path,
    pub(crate) output: &'a Path,
    pub(crate) selected: Option<&'a str>,
    pub(crate) manual_trace: Option<&'a Path>,
    pub(crate) run_configured: bool,
}

pub(crate) fn run(request: &RunRequest<'_>, graph: &mut EvidenceGraph) -> Result<RuntimeSummary> {
    let traces = request.output.join("traces");
    let logs = request.output.join("logs");
    fs::create_dir_all(&traces)
        .with_context(|| format!("create trace directory: {}", traces.display()))?;
    fs::create_dir_all(&logs)
        .with_context(|| format!("create scenario log directory: {}", logs.display()))?;

    let scenarios = if request.run_configured {
        select_scenarios(request.config, request.selected)?
    } else if let Some(selected) = request.selected {
        bail!("cannot select runtime scenario `{selected}` when runtime is off");
    } else {
        Vec::new()
    };
    let mut summaries = Vec::new();
    for scenario in scenarios {
        summaries.push(run_scenario(request, scenario, &traces, &logs, graph)?);
    }
    if let Some(path) = request.manual_trace {
        let destination = traces.join("manual.jsonl");
        fs::copy(path, &destination).with_context(|| {
            format!(
                "copy manual architecture trace {} to {}",
                path.display(),
                destination.display()
            )
        })?;
        let trace = trace::import(&destination, "manual", graph, true);
        summaries.push(ScenarioSummary {
            name: "manual".to_string(),
            state: if trace.state == TraceState::Failed {
                ScenarioState::Failed
            } else {
                ScenarioState::Manual
            },
            exit_code: None,
            trace,
            stdout: None,
            stderr: None,
        });
    }
    Ok(RuntimeSummary {
        scenarios: summaries,
    })
}

fn select_scenarios<'a>(
    config: &'a ArchitectureConfig,
    selected: Option<&str>,
) -> Result<Vec<&'a RuntimeScenarioConfig>> {
    if let Some(selected) = selected {
        let scenario = config
            .runtime
            .scenarios
            .iter()
            .find(|scenario| scenario.name() == selected)
            .ok_or_else(|| {
                anyhow::anyhow!("architecture runtime scenario not found: {selected}")
            })?;
        Ok(vec![scenario])
    } else {
        Ok(config.runtime.scenarios.iter().collect())
    }
}

fn run_scenario(
    request: &RunRequest<'_>,
    scenario: &RuntimeScenarioConfig,
    traces: &Path,
    logs: &Path,
    graph: &mut EvidenceGraph,
) -> Result<ScenarioSummary> {
    let name = scenario.name();
    match scenario {
        RuntimeScenarioConfig::Trace { path, .. } => {
            let trace = trace::import(&request.root.join(path), name, graph, false);
            Ok(ScenarioSummary {
                name: name.to_string(),
                state: scenario_state(trace.state),
                exit_code: None,
                trace,
                stdout: None,
                stderr: None,
            })
        }
        RuntimeScenarioConfig::Test { timeout_secs, .. }
        | RuntimeScenarioConfig::Binary { timeout_secs, .. } => {
            validate_target(request.metadata, scenario)?;
            let trace_path = traces.join(format!("{name}.jsonl"));
            if trace_path.exists() {
                fs::remove_file(&trace_path)
                    .with_context(|| format!("reset trace: {}", trace_path.display()))?;
            }
            let stdout_path = logs.join(format!("{name}.stdout.log"));
            let stderr_path = logs.join(format!("{name}.stderr.log"));
            let args = cargo_args(scenario);
            let environment = [(TRACE_ENV, trace_path.as_os_str())];
            let outcome = run_process_with_env(
                &ProcessRequest {
                    program: "cargo",
                    args: &args,
                    cwd: request.root,
                    stdout_path: &stdout_path,
                    stderr_path: &stderr_path,
                    timeout: Duration::from_secs(*timeout_secs),
                },
                &environment,
            )
            .map_err(|error| anyhow::anyhow!("run architecture scenario `{name}`: {error}"))?;
            let trace = if trace_path.is_file() {
                trace::import(&trace_path, name, graph, false)
            } else {
                TraceSummary {
                    state: TraceState::Empty,
                    records: 0,
                    matched_records: 0,
                    unmatched_records: 0,
                    diagnostics: vec!["scenario did not create a trace".to_string()],
                }
            };
            let state = if outcome.timed_out {
                ScenarioState::TimedOut
            } else if !outcome.status.success() {
                ScenarioState::Failed
            } else {
                scenario_state(trace.state)
            };
            Ok(ScenarioSummary {
                name: name.to_string(),
                state,
                exit_code: outcome.status.code(),
                trace,
                stdout: Some(relative(request.output, &stdout_path)),
                stderr: Some(relative(request.output, &stderr_path)),
            })
        }
    }
}

fn validate_target(metadata: &Metadata, scenario: &RuntimeScenarioConfig) -> Result<()> {
    let (package_name, target_name, kind) = match scenario {
        RuntimeScenarioConfig::Test { package, test, .. } => (package, test, TargetKind::Test),
        RuntimeScenarioConfig::Binary { package, bin, .. } => (package, bin, TargetKind::Bin),
        RuntimeScenarioConfig::Trace { .. } => return Ok(()),
    };
    let package = workspace_package(metadata, package_name).ok_or_else(|| {
        anyhow::anyhow!(
            "architecture scenario `{}` package not found: {package_name}",
            scenario.name()
        )
    })?;
    if !package
        .targets
        .iter()
        .any(|target| target.name == *target_name && target.kind.contains(&kind))
    {
        bail!(
            "architecture scenario `{}` target not found: {target_name}",
            scenario.name()
        );
    }
    Ok(())
}

fn workspace_package<'a>(metadata: &'a Metadata, name: &str) -> Option<&'a Package> {
    metadata
        .workspace_packages()
        .into_iter()
        .find(|package| package.name == name)
}

fn cargo_args(scenario: &RuntimeScenarioConfig) -> Vec<String> {
    match scenario {
        RuntimeScenarioConfig::Test {
            package,
            test,
            filter,
            features,
            ignored,
            ..
        } => {
            let mut args = vec![
                "test".to_string(),
                "-p".to_string(),
                package.clone(),
                "--test".to_string(),
                test.clone(),
            ];
            add_features(&mut args, features);
            if let Some(filter) = filter {
                args.push(filter.clone());
            }
            args.push("--".to_string());
            if *ignored {
                args.push("--ignored".to_string());
            }
            args.push("--nocapture".to_string());
            args
        }
        RuntimeScenarioConfig::Binary {
            package,
            bin,
            args: binary_args,
            features,
            ..
        } => {
            let mut args = vec![
                "run".to_string(),
                "-p".to_string(),
                package.clone(),
                "--bin".to_string(),
                bin.clone(),
            ];
            add_features(&mut args, features);
            if !binary_args.is_empty() {
                args.push("--".to_string());
                args.extend(binary_args.iter().cloned());
            }
            args
        }
        RuntimeScenarioConfig::Trace { .. } => Vec::new(),
    }
}

fn add_features(args: &mut Vec<String>, features: &[String]) {
    if !features.is_empty() {
        args.extend(["--features".to_string(), features.join(",")]);
    }
}

fn scenario_state(state: TraceState) -> ScenarioState {
    match state {
        TraceState::Complete => ScenarioState::Complete,
        TraceState::Empty => ScenarioState::Empty,
        TraceState::Truncated => ScenarioState::Truncated,
        TraceState::Failed => ScenarioState::Failed,
    }
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cargo_test_scenario_uses_structured_arguments_without_a_shell() {
        let scenario = RuntimeScenarioConfig::Test {
            name: "demo".to_string(),
            package: "demo-package".to_string(),
            test: "architecture".to_string(),
            filter: Some("flow".to_string()),
            features: vec!["probe".to_string(), "fixture".to_string()],
            ignored: false,
            timeout_secs: 30,
        };

        assert_eq!(
            cargo_args(&scenario),
            [
                "test",
                "-p",
                "demo-package",
                "--test",
                "architecture",
                "--features",
                "probe,fixture",
                "flow",
                "--",
                "--nocapture"
            ]
        );
    }

    #[test]
    fn selecting_an_unknown_scenario_is_an_error() {
        let config = ArchitectureConfig::default();
        assert!(select_scenarios(&config, Some("missing")).is_err());
    }
}

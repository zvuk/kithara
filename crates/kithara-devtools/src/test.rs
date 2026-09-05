use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    process::Command,
};

use anyhow::{Context, Result, bail};
use clap::Args;
use serde::{Deserialize, Serialize};

use crate::{
    common::project::{ProjectConfig, TestCommandConfig, TestLaneConfig},
    sccache, touched,
    verdict::ChildFailure,
};

#[derive(Debug, Args)]
#[command(trailing_var_arg = true)]
pub struct TestArgs {
    /// Arguments for the configured test command. Recipe-level flags accepted anywhere:
    /// `--lane=<configured-name>`, `--touched`, `--flash=true|false|on|off`, `--no-flash`,
    /// `--loom=true|false|on|off`, `--no-loom`, `--no-block=true|false|on|off`, and
    /// `--net-backend=<configured-name>`.
    #[arg(value_name = "ARGS", allow_hyphen_values = true)]
    pub(crate) args: Vec<String>,
}

#[derive(Debug)]
struct TestRequest {
    flash: Option<bool>,
    lane: Option<String>,
    loom: Option<bool>,
    net_backend: Option<String>,
    no_block: Option<bool>,
    passthrough: Vec<String>,
    touched: bool,
}

impl TestRequest {
    fn parse(args: &[String]) -> Result<Self> {
        let mut request = Self {
            lane: None,
            no_block: None,
            loom: None,
            net_backend: None,
            passthrough: Vec::new(),
            flash: None,
            touched: false,
        };
        let mut iter = args.iter();
        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--flash=off" | "--flash=false" | "--no-flash" => request.flash = Some(false),
                "--flash=on" | "--flash=true" => request.flash = Some(true),
                "--flash" => {
                    let value = iter
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("--flash requires a value"))?;
                    request.flash = Some(parse_toggle("flash", value)?);
                }
                "--no-block=off" | "--no-block=false" => request.no_block = Some(false),
                "--no-block=on" | "--no-block=true" => request.no_block = Some(true),
                "--no-block" => {
                    let value = iter
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("--no-block requires a value"))?;
                    request.no_block = Some(parse_toggle("no-block", value)?);
                }
                "--touched" => request.touched = true,
                "--loom=off" | "--loom=false" | "--no-loom" => request.loom = Some(false),
                "--loom=on" | "--loom=true" => request.loom = Some(true),
                "--loom" => {
                    let value = iter
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("--loom requires a value"))?;
                    request.loom = Some(parse_toggle("loom", value)?);
                }
                "--lane" => {
                    let value = iter
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("--lane requires a value"))?;
                    request.lane = Some(value.clone());
                }
                "--net-backend" => {
                    let value = iter
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("--net-backend requires a value"))?;
                    request.net_backend = Some(value.clone());
                }
                _ if arg.starts_with("--flash=") => {
                    let value = arg.trim_start_matches("--flash=");
                    request.flash = Some(parse_toggle("flash", value)?);
                }
                _ if arg.starts_with("--no-block=") => {
                    let value = arg.trim_start_matches("--no-block=");
                    request.no_block = Some(parse_toggle("no-block", value)?);
                }
                _ if arg.starts_with("--loom=") => {
                    let value = arg.trim_start_matches("--loom=");
                    request.loom = Some(parse_toggle("loom", value)?);
                }
                _ if arg.starts_with("--lane=") => {
                    let value = arg.trim_start_matches("--lane=");
                    request.lane = Some(value.to_owned());
                }
                _ if arg.starts_with("--net-backend=") => {
                    let value = arg.trim_start_matches("--net-backend=");
                    request.net_backend = Some(value.to_owned());
                }
                _ => request.passthrough.push(arg.clone()),
            }
        }
        Ok(request)
    }
}

pub(crate) fn run(args: &TestArgs) -> Result<()> {
    let request = TestRequest::parse(&args.args)?;
    let project = ProjectConfig::load(Path::new("."))?;
    let test = &project.test;
    validate_config(test)?;

    if request.touched {
        return run_touched(&project, &request);
    }
    let (lane_name, lane) = select_lane(test, &request)?;
    run_lane(&project, lane_name, lane, &request)
}

/// Run every lane the branch touched, serially, without letting the first
/// failure hide the rest: this exists to name which lane broke.
fn run_touched(project: &ProjectConfig, request: &TestRequest) -> Result<()> {
    if request.lane.is_some() {
        bail!("--touched selects its own lanes and conflicts with --lane");
    }
    let test = &project.test;
    let selected = touched::lanes(test)?;
    if selected.is_empty() {
        println!("no owned path touched; the nightly sweep covers these lanes");
        return Ok(());
    }
    let mut failed = Vec::new();
    for lane_name in &selected {
        let lane = test
            .lanes
            .get(lane_name)
            .with_context(|| format!("test lane `{lane_name}` is not configured"))?;
        println!("=== {lane_name} ===");
        if run_lane(project, lane_name, lane, request).is_err() {
            failed.push(lane_name.clone());
        }
    }
    if !failed.is_empty() {
        bail!("touched test lanes failed: {}", failed.join(", "));
    }
    Ok(())
}

fn run_lane(
    project: &ProjectConfig,
    lane_name: &str,
    lane: &TestLaneConfig,
    request: &TestRequest,
) -> Result<()> {
    let mut cmd = lane_command(project, lane_name, lane, request)?;

    let status = cmd
        .status()
        .with_context(|| format!("failed to run test lane `{lane_name}`: {}", lane.program))?;
    // Before the verdict rather than after it: a red lane is exactly when the
    // build's share of the wall clock needs explaining, and reporting after the
    // early return would print the number only for lanes that passed.
    sccache::report_stats(project.tools.program("sccache"));
    if !status.success() {
        return Err(ChildFailure::inherited(
            format!("test lane `{lane_name}`"),
            status.code(),
        ));
    }
    Ok(())
}

fn lane_command(
    project: &ProjectConfig,
    lane_name: &str,
    lane: &TestLaneConfig,
    request: &TestRequest,
) -> Result<Command> {
    let test = &project.test;
    let passthrough = passthrough_position(lane)?;
    match passthrough {
        PassthroughPosition::BeforeSuffix if lane_name == test.default_lane => {
            let toggles = LaneToggles {
                flash: request
                    .flash
                    .unwrap_or_else(|| lane.default_flash.unwrap_or(test.flash.default)),
                no_block: request
                    .no_block
                    .unwrap_or_else(|| lane.default_no_block.unwrap_or(test.no_block.default)),
            };
            let backend = request
                .net_backend
                .as_deref()
                .unwrap_or(&test.default_backend);
            let (_, cmd) = nextest_lane_command(project, toggles, backend, &request.passthrough)?;
            Ok(cmd)
        }
        passthrough => {
            let mut cmd = Command::new(&lane.program);
            cmd.envs(&lane.env);
            cmd.args(&lane.prefix_args);
            let features = features_for(test, lane, request)?;
            if !features.is_empty() {
                cmd.arg(&test.feature_arg)
                    .arg(features.into_iter().collect::<Vec<_>>().join(","));
            }
            match passthrough {
                PassthroughPosition::BeforeSuffix => {
                    cmd.args(&request.passthrough);
                    cmd.args(&lane.suffix_args);
                }
                PassthroughPosition::AfterSuffix => {
                    cmd.args(&lane.suffix_args);
                    cmd.args(&request.passthrough);
                }
            }
            Ok(cmd)
        }
    }
}

fn validate_config(config: &TestCommandConfig) -> Result<()> {
    if config.default_lane.is_empty() {
        bail!("missing test.default_lane in .config/xtask.toml");
    }
    if config.default_backend.is_empty() {
        bail!("missing test.default_backend in .config/xtask.toml");
    }
    if config.feature_arg.is_empty() {
        bail!("missing test.feature_arg in .config/xtask.toml");
    }
    if !config.lanes.contains_key(&config.default_lane) {
        bail!(
            "test.default_lane `{}` is not defined in test.lanes",
            config.default_lane
        );
    }
    if !config.loom_lane.is_empty() && !config.lanes.contains_key(&config.loom_lane) {
        bail!(
            "test.loom_lane `{}` is not defined in test.lanes",
            config.loom_lane
        );
    }
    if !config.net_backends.contains_key(&config.default_backend) {
        bail!(
            "test.default_backend `{}` is not defined in test.net_backends",
            config.default_backend
        );
    }
    for (name, lane) in &config.lanes {
        if lane.program.is_empty() {
            bail!("test.lanes.{name}.program is empty");
        }
        passthrough_position(lane).with_context(|| format!("test.lanes.{name}.passthrough"))?;
    }
    Ok(())
}

fn select_lane<'a>(
    config: &'a TestCommandConfig,
    request: &'a TestRequest,
) -> Result<(&'a str, &'a TestLaneConfig)> {
    let explicit_lane = request.lane.as_deref();
    let lane_name = match request.loom {
        Some(true) => {
            if config.loom_lane.is_empty() {
                bail!("--loom=on requires test.loom_lane in .config/xtask.toml");
            }
            if let Some(explicit_lane) = explicit_lane
                && explicit_lane != config.loom_lane
            {
                bail!(
                    "--loom=on selects lane `{}` and conflicts with --lane={explicit_lane}",
                    config.loom_lane
                );
            }
            config.loom_lane.as_str()
        }
        Some(false)
            if explicit_lane == Some(config.loom_lane.as_str()) && !config.loom_lane.is_empty() =>
        {
            bail!("--loom=off conflicts with --lane={}", config.loom_lane);
        }
        Some(false) | None => explicit_lane.unwrap_or(&config.default_lane),
    };
    let Some(lane) = config.lanes.get(lane_name) else {
        let valid = config
            .lanes
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        bail!("unsupported test lane `{lane_name}`; configured values: {valid}");
    };
    Ok((lane_name, lane))
}

#[derive(Clone, Copy)]
pub(crate) struct LaneToggles {
    pub(crate) flash: bool,
    pub(crate) no_block: bool,
}

fn features_for(
    config: &TestCommandConfig,
    lane: &TestLaneConfig,
    request: &TestRequest,
) -> Result<BTreeSet<String>> {
    let flash = request
        .flash
        .unwrap_or_else(|| lane.default_flash.unwrap_or(config.flash.default));
    let no_block = request
        .no_block
        .unwrap_or_else(|| lane.default_no_block.unwrap_or(config.no_block.default));
    let backend_name = request
        .net_backend
        .clone()
        .unwrap_or_else(|| config.default_backend.clone());
    lane_features(config, lane, LaneToggles { flash, no_block }, &backend_name)
}

pub(crate) fn lane_features(
    config: &TestCommandConfig,
    lane: &TestLaneConfig,
    toggles: LaneToggles,
    backend_name: &str,
) -> Result<BTreeSet<String>> {
    let mut features = BTreeSet::new();
    features.extend(config.features.iter().cloned());
    features.extend(lane.default_features.iter().cloned());
    if toggles.flash {
        features.extend(config.flash.features.iter().cloned());
    }
    if toggles.no_block {
        features.extend(config.no_block.features.iter().cloned());
    }
    let Some(backend) = config.net_backends.get(backend_name) else {
        let valid = config
            .net_backends
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        bail!("unsupported net backend `{backend_name}`; configured values: {valid}");
    };
    features.extend(backend.features.iter().cloned());
    Ok(features)
}

pub(crate) fn nextest_lane_command(
    project: &ProjectConfig,
    toggles: LaneToggles,
    backend: &str,
    extra: &[String],
) -> Result<(Vec<String>, Command)> {
    nextest_lane_command_for(project, toggles, backend, extra, NextestAction::Run)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NextestAction {
    Run,
    List,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ConfiguredLane {
    /// A lane with no environment of its own records none, so a campaign that
    /// predates lane environments keeps comparing against the same runner.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) env: BTreeMap<String, String>,
    pub(crate) backend: String,
    pub(crate) feature_arg: String,
    pub(crate) lane: String,
    pub(crate) program: String,
    pub(crate) features: Vec<String>,
    pub(crate) prefix_args: Vec<String>,
    pub(crate) suffix_args: Vec<String>,
}

pub(crate) fn nextest_lane_command_for(
    project: &ProjectConfig,
    toggles: LaneToggles,
    backend: &str,
    extra: &[String],
    action: NextestAction,
) -> Result<(Vec<String>, Command)> {
    let test = &project.test;
    validate_config(test)?;
    let lane_name = &test.default_lane;
    let Some(lane) = test.lanes.get(lane_name) else {
        bail!("test.default_lane `{lane_name}` is not defined in test.lanes");
    };
    let features = lane_features(test, lane, toggles, backend)?;
    nextest_command(test, lane, features, extra, action)
}

pub(crate) fn nextest_configured_lane_command(
    resolved: &ConfiguredLane,
    extra: &[String],
    action: NextestAction,
) -> Result<(Vec<String>, Command)> {
    resolved.validate()?;
    build_nextest_command(
        NextestSpec {
            program: &resolved.program,
            prefix_args: &resolved.prefix_args,
            suffix_args: &resolved.suffix_args,
            feature_arg: &resolved.feature_arg,
            env: &resolved.env,
        },
        resolved.features.iter().cloned().collect(),
        extra,
        action,
    )
}

pub(crate) fn configured_lane(
    project: &ProjectConfig,
    lane_name: &str,
    backend_name: &str,
    additional_features: &[String],
) -> Result<ConfiguredLane> {
    let test = &project.test;
    validate_config(test)?;
    let lane = test
        .lanes
        .get(lane_name)
        .with_context(|| format!("stress lane `{lane_name}` is not configured"))?;
    let mut features = BTreeSet::new();
    features.extend(test.features.iter().cloned());
    features.extend(lane.default_features.iter().cloned());
    features.extend(additional_features.iter().cloned());
    let backend = test
        .net_backends
        .get(backend_name)
        .with_context(|| format!("stress backend `{backend_name}` is not configured"))?;
    features.extend(backend.features.iter().cloned());
    Ok(ConfiguredLane {
        lane: lane_name.to_owned(),
        backend: backend_name.to_owned(),
        program: lane.program.clone(),
        prefix_args: lane.prefix_args.clone(),
        suffix_args: lane.suffix_args.clone(),
        feature_arg: test.feature_arg.clone(),
        features: features.into_iter().collect(),
        env: lane.env.clone(),
    })
}

impl ConfiguredLane {
    pub(crate) fn validate(&self) -> Result<()> {
        for (field, value) in [
            ("lane", self.lane.as_str()),
            ("backend", self.backend.as_str()),
            ("program", self.program.as_str()),
            ("feature argument", self.feature_arg.as_str()),
        ] {
            if value.trim().is_empty() {
                bail!("configured test runner {field} is empty");
            }
        }
        let mut features = BTreeSet::new();
        for feature in &self.features {
            if feature.trim().is_empty() {
                bail!("configured test runner contains an empty feature");
            }
            if !features.insert(feature) {
                bail!("configured test runner contains duplicate feature `{feature}`");
            }
        }
        Ok(())
    }
}

/// What a lane runs, resolved from either the lane config or a configured
/// runner recorded by a campaign.
#[derive(Clone, Copy)]
struct NextestSpec<'a> {
    env: &'a BTreeMap<String, String>,
    prefix_args: &'a [String],
    suffix_args: &'a [String],
    feature_arg: &'a str,
    program: &'a str,
}

fn nextest_command(
    test: &TestCommandConfig,
    lane: &TestLaneConfig,
    features: BTreeSet<String>,
    extra: &[String],
    action: NextestAction,
) -> Result<(Vec<String>, Command)> {
    build_nextest_command(
        NextestSpec {
            program: &lane.program,
            prefix_args: &lane.prefix_args,
            suffix_args: &lane.suffix_args,
            feature_arg: &test.feature_arg,
            env: &lane.env,
        },
        features,
        extra,
        action,
    )
}

fn build_nextest_command(
    spec: NextestSpec<'_>,
    features: BTreeSet<String>,
    extra: &[String],
    action: NextestAction,
) -> Result<(Vec<String>, Command)> {
    let NextestSpec {
        program,
        prefix_args,
        suffix_args,
        feature_arg,
        env,
    } = spec;
    let mut cmd = Command::new(program);
    cmd.envs(env);
    match action {
        NextestAction::Run => {
            cmd.args(prefix_args);
        }
        NextestAction::List => {
            let mut prefix_args = prefix_args.to_vec();
            let nextest_index = prefix_args
                .iter()
                .position(|arg| arg == "nextest")
                .context("default test lane must contain `nextest` for list inventory")?;
            let action_index = prefix_args
                .iter()
                .enumerate()
                .skip(nextest_index + 1)
                .find_map(|(index, arg)| (arg == "run").then_some(index))
                .context("default test lane must contain a `run` action after `nextest`")?;
            prefix_args[action_index] = "list".to_owned();
            cmd.args(prefix_args);
        }
    }
    if !features.is_empty() {
        cmd.arg(feature_arg)
            .arg(features.iter().cloned().collect::<Vec<_>>().join(","));
    }
    cmd.args(extra);
    cmd.args(suffix_args);
    Ok((features.into_iter().collect(), cmd))
}

enum PassthroughPosition {
    BeforeSuffix,
    AfterSuffix,
}

fn passthrough_position(lane: &TestLaneConfig) -> Result<PassthroughPosition> {
    match lane.passthrough.as_str() {
        "" | "before-suffix" => Ok(PassthroughPosition::BeforeSuffix),
        "after-suffix" => Ok(PassthroughPosition::AfterSuffix),
        value => bail!("unsupported passthrough position `{value}`"),
    }
}

fn parse_toggle(name: &str, value: &str) -> Result<bool> {
    match value {
        "on" | "true" => Ok(true),
        "off" | "false" => Ok(false),
        _ => bail!("unsupported {name} mode: {value}"),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::common::project::{
        AuditClippyConfig, HealthConfig, LintExcludeConfig, OrphansConfig, PerfConfig,
        ProjectIdentity, QualityConfig, StressConfig, TestFlashConfig, TestNetBackendConfig,
        TestNoBlockConfig, WorkspaceScan,
    };

    fn args_of(cmd: &Command) -> Vec<String> {
        cmd.get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    fn envs_of(cmd: &Command) -> Vec<(String, String)> {
        cmd.get_envs()
            .filter_map(|(key, value)| {
                value.map(|value| {
                    (
                        key.to_string_lossy().into_owned(),
                        value.to_string_lossy().into_owned(),
                    )
                })
            })
            .collect()
    }

    fn synthetic_project() -> ProjectConfig {
        let mut lanes = BTreeMap::new();
        lanes.insert(
            "workspace".to_owned(),
            TestLaneConfig {
                program: "cargo".to_owned(),
                prefix_args: vec![
                    "nextest".to_owned(),
                    "run".to_owned(),
                    "--workspace".to_owned(),
                ],
                suffix_args: vec!["--locked".to_owned()],
                default_features: Vec::new(),
                default_flash: None,
                default_no_block: None,
                passthrough: String::new(),
                env: BTreeMap::new(),
                owns: Vec::new(),
            },
        );
        lanes.insert(
            "loom".to_owned(),
            TestLaneConfig {
                program: "cargo".to_owned(),
                prefix_args: vec![
                    "nextest".to_owned(),
                    "run".to_owned(),
                    "--workspace".to_owned(),
                ],
                suffix_args: vec!["-E".to_owned(), "test(loom_model_)".to_owned()],
                default_features: vec!["demo/loom".to_owned()],
                default_flash: Some(false),
                default_no_block: None,
                passthrough: String::new(),
                env: BTreeMap::new(),
                owns: Vec::new(),
            },
        );
        lanes.insert(
            "detector".to_owned(),
            TestLaneConfig {
                program: "cargo".to_owned(),
                prefix_args: vec!["nextest".to_owned(), "run".to_owned()],
                suffix_args: Vec::new(),
                default_features: Vec::new(),
                default_flash: None,
                default_no_block: Some(true),
                passthrough: String::new(),
                env: BTreeMap::new(),
                owns: Vec::new(),
            },
        );
        lanes.insert(
            "browser".to_owned(),
            TestLaneConfig {
                program: "cargo".to_owned(),
                prefix_args: vec!["test".to_owned()],
                suffix_args: vec!["selenium".to_owned()],
                default_features: Vec::new(),
                default_flash: Some(false),
                default_no_block: None,
                passthrough: "after-suffix".to_owned(),
                env: BTreeMap::from([("DEMO_BROWSER".to_owned(), "firefox".to_owned())]),
                owns: Vec::new(),
            },
        );
        let mut net_backends = BTreeMap::new();
        net_backends.insert(
            "http".to_owned(),
            TestNetBackendConfig {
                features: Vec::new(),
            },
        );
        net_backends.insert(
            "native".to_owned(),
            TestNetBackendConfig {
                features: vec!["demo/native-net".to_owned()],
            },
        );
        ProjectConfig {
            architecture: crate::common::project::ArchitectureConfig::default(),
            project: ProjectIdentity {
                name: "demo".to_owned(),
            },
            audit_clippy: AuditClippyConfig::default(),
            ci_report: crate::common::project::CiReportConfig::default(),
            health: HealthConfig::default(),
            test: TestCommandConfig {
                lanes,
                net_backends,
                shared_paths: Vec::new(),
                default_lane: "workspace".to_owned(),
                default_backend: "http".to_owned(),
                feature_arg: "--features".to_owned(),
                features: vec!["base-feature".to_owned()],
                flash: TestFlashConfig {
                    features: vec!["virtual-time".to_owned()],
                    default: true,
                },
                no_block: TestNoBlockConfig {
                    features: vec!["nb-detect".to_owned()],
                    default: false,
                },
                loom_lane: "loom".to_owned(),
            },
            lint_exclude: LintExcludeConfig::default(),
            workspace_scan: WorkspaceScan::default(),
            orphans: OrphansConfig::default(),
            quality: QualityConfig::default(),
            perf: PerfConfig::default(),
            stress: StressConfig::default(),
            ext: toml::Table::default(),
            tools: crate::common::tools::ToolsConfig::default(),
        }
    }

    #[test]
    fn lane_features_flash_and_backend() {
        let project = synthetic_project();
        let test = &project.test;
        let lane = &test.lanes[&test.default_lane];

        let feats = lane_features(
            test,
            lane,
            LaneToggles {
                flash: true,
                no_block: false,
            },
            "native",
        )
        .expect("features");
        assert!(feats.contains("base-feature"));
        assert!(feats.contains("virtual-time"));
        assert!(feats.contains("demo/native-net"));
        assert!(!feats.contains("nb-detect"));

        let feats = lane_features(
            test,
            lane,
            LaneToggles {
                flash: false,
                no_block: false,
            },
            "http",
        )
        .expect("features");
        assert_eq!(feats, BTreeSet::from(["base-feature".to_owned()]));
    }

    #[test]
    fn features_default_request_omits_no_block() {
        let project = synthetic_project();
        let test = &project.test;
        let lane = &test.lanes[&test.default_lane];
        let request = TestRequest::parse(&[]).expect("parse request");

        let feats = features_for(test, lane, &request).expect("features");
        assert!(!feats.contains("nb-detect"));
    }

    #[test]
    fn no_block_on_adds_features_and_composes_with_flash_and_backend() {
        let project = synthetic_project();
        let test = &project.test;
        let lane = &test.lanes[&test.default_lane];
        let request = TestRequest::parse(&[
            "--flash=on".to_owned(),
            "--no-block=on".to_owned(),
            "--net-backend=native".to_owned(),
        ])
        .expect("parse request");

        let feats = features_for(test, lane, &request).expect("features");
        assert!(feats.contains("base-feature"));
        assert!(feats.contains("virtual-time"));
        assert!(feats.contains("demo/native-net"));
        assert!(feats.contains("nb-detect"));
    }

    #[test]
    fn no_block_off_keeps_no_block_features_out() {
        let project = synthetic_project();
        let test = &project.test;
        let lane = &test.lanes[&test.default_lane];
        let request = TestRequest::parse(&[
            "--flash=on".to_owned(),
            "--no-block=off".to_owned(),
            "--net-backend=native".to_owned(),
        ])
        .expect("parse request");

        let feats = features_for(test, lane, &request).expect("features");
        assert!(!feats.contains("nb-detect"));
        assert!(feats.contains("virtual-time"));
    }

    #[test]
    fn a_lane_that_asks_for_the_detector_gets_it_without_a_flag() {
        let project = synthetic_project();
        let test = &project.test;
        let lane = &test.lanes["detector"];
        let request = TestRequest::parse(&[]).expect("parse request");

        let feats = features_for(test, lane, &request).expect("features");

        assert!(feats.contains("nb-detect"));
    }

    #[test]
    fn an_explicit_off_overrides_the_lane_detector_default() {
        let project = synthetic_project();
        let test = &project.test;
        let lane = &test.lanes["detector"];
        let request = TestRequest::parse(&["--no-block=off".to_owned()]).expect("parse request");

        let feats = features_for(test, lane, &request).expect("features");

        assert!(!feats.contains("nb-detect"));
    }

    #[test]
    fn no_block_bogus_mode_is_a_typed_error() {
        let error = TestRequest::parse(&["--no-block".to_owned(), "bogus".to_owned()])
            .expect_err("parse invalid no-block");

        assert!(error.to_string().contains("no-block"));
    }

    #[test]
    fn no_block_space_form_parses_to_on() {
        let project = synthetic_project();
        let test = &project.test;
        let lane = &test.lanes[&test.default_lane];
        let request =
            TestRequest::parse(&["--no-block".to_owned(), "on".to_owned()]).expect("parse request");

        let feats = features_for(test, lane, &request).expect("features");
        assert!(feats.contains("nb-detect"));
    }

    #[test]
    fn nextest_lane_command_shape() {
        let project = synthetic_project();
        let extra = vec!["--profile".to_owned(), "perf".to_owned()];

        let (features, cmd) = nextest_lane_command(
            &project,
            LaneToggles {
                flash: true,
                no_block: false,
            },
            "http",
            &extra,
        )
        .expect("nextest command");

        assert_eq!(
            features,
            vec!["base-feature".to_owned(), "virtual-time".to_owned()]
        );
        let args = args_of(&cmd);
        assert_eq!(cmd.get_program().to_string_lossy(), "cargo");
        assert!(args.windows(2).any(|w| w == ["nextest", "run"]));
        assert!(args.windows(2).any(|w| w == ["--profile", "perf"]));
        assert!(args.contains(&"--workspace".to_owned()));
        assert_eq!(args.last().map(String::as_str), Some("--locked"));
    }

    #[test]
    fn nextest_run_preserves_prefix_with_global_args() {
        let mut project = synthetic_project();
        let prefix_args = vec![
            "nextest".to_owned(),
            "--color".to_owned(),
            "always".to_owned(),
            "run".to_owned(),
            "--workspace".to_owned(),
        ];
        let lane = project
            .test
            .lanes
            .get_mut("workspace")
            .expect("default lane");
        lane.prefix_args.clone_from(&prefix_args);

        let (_, cmd) = nextest_lane_command(
            &project,
            LaneToggles {
                flash: true,
                no_block: false,
            },
            "http",
            &[],
        )
        .expect("nextest run command");
        let args = args_of(&cmd);

        assert_eq!(&args[..prefix_args.len()], prefix_args.as_slice());
    }

    #[test]
    fn nextest_inventory_replaces_run_after_global_args() {
        let mut project = synthetic_project();
        let lane = project
            .test
            .lanes
            .get_mut("workspace")
            .expect("default lane");
        lane.prefix_args = vec![
            "nextest".to_owned(),
            "--color".to_owned(),
            "always".to_owned(),
            "run".to_owned(),
            "--workspace".to_owned(),
        ];

        let (_, cmd) = nextest_lane_command_for(
            &project,
            LaneToggles {
                flash: true,
                no_block: true,
            },
            "http",
            &["--message-format".to_owned(), "json".to_owned()],
            NextestAction::List,
        )
        .expect("nextest list command");
        let args = args_of(&cmd);

        let expected = ["nextest", "--color", "always", "list", "--workspace"].map(str::to_owned);
        assert_eq!(&args[..expected.len()], expected.as_slice());
        assert!(args.windows(2).any(|w| w == ["--message-format", "json"]));
        assert!(
            args.iter()
                .any(|arg| arg.split(',').any(|feature| feature == "nb-detect"))
        );
    }

    #[test]
    fn loom_flag_selects_model_lane_and_composes_with_flash() {
        let project = synthetic_project();
        let request = TestRequest::parse(&["--loom=on".to_owned(), "--flash=on".to_owned()])
            .expect("parse request");

        let (name, lane) = select_lane(&project.test, &request).expect("select loom lane");
        assert_eq!(name, "loom");
        let features = features_for(&project.test, lane, &request).expect("loom features");
        assert_eq!(
            features,
            BTreeSet::from([
                "base-feature".to_owned(),
                "demo/loom".to_owned(),
                "virtual-time".to_owned(),
            ])
        );
    }

    #[test]
    fn loom_flag_with_no_block_on_composes_features() {
        let project = synthetic_project();
        let request = TestRequest::parse(&["--loom=on".to_owned(), "--no-block=on".to_owned()])
            .expect("parse request");

        let (name, lane) = select_lane(&project.test, &request).expect("select loom lane");
        assert_eq!(name, "loom");
        let features = features_for(&project.test, lane, &request).expect("loom features");
        assert_eq!(
            features,
            BTreeSet::from([
                "base-feature".to_owned(),
                "demo/loom".to_owned(),
                "nb-detect".to_owned(),
            ])
        );
    }

    #[test]
    fn loom_flag_rejects_an_explicit_non_model_lane() {
        let project = synthetic_project();
        let request = TestRequest::parse(&["--loom=on".to_owned(), "--lane=workspace".to_owned()])
            .expect("parse request");

        let error = select_lane(&project.test, &request).expect_err("lane conflict");
        assert!(
            error
                .to_string()
                .contains("conflicts with --lane=workspace")
        );
    }

    #[test]
    fn a_lane_that_names_an_environment_runs_with_it() {
        let project = synthetic_project();
        let request = TestRequest::parse(&["--lane=browser".to_owned()]).expect("parse request");
        let (name, lane) = select_lane(&project.test, &request).expect("select browser lane");

        let cmd = lane_command(&project, name, lane, &request).expect("browser lane command");

        assert_eq!(
            envs_of(&cmd),
            vec![("DEMO_BROWSER".to_owned(), "firefox".to_owned())]
        );
    }

    #[test]
    fn a_lane_that_names_none_runs_with_none() {
        let project = synthetic_project();
        let request = TestRequest::parse(&[]).expect("parse request");
        let (name, lane) = select_lane(&project.test, &request).expect("select default lane");

        let cmd = lane_command(&project, name, lane, &request).expect("default lane command");

        assert!(envs_of(&cmd).is_empty());
    }

    #[test]
    fn a_configured_lane_carries_its_environment_to_the_runner() {
        let project = synthetic_project();

        let resolved = configured_lane(&project, "browser", "http", &[]).expect("configured lane");

        assert_eq!(resolved.env["DEMO_BROWSER"], "firefox");
        let (_, cmd) =
            nextest_configured_lane_command(&resolved, &[], NextestAction::Run).expect("command");
        assert_eq!(
            envs_of(&cmd),
            vec![("DEMO_BROWSER".to_owned(), "firefox".to_owned())]
        );
    }
}

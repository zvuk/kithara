use std::{collections::BTreeSet, path::Path, process::Command};

use anyhow::{Context, Result, bail};
use clap::Args;

use crate::common::project::{ProjectConfig, TestCommandConfig, TestLaneConfig};

#[derive(Debug, Args)]
#[command(trailing_var_arg = true)]
pub struct TestArgs {
    /// Arguments for the configured test command. Recipe-level flags accepted anywhere:
    /// `--lane=<configured-name>`, `--flash=true|false|on|off`, `--no-flash`,
    /// and `--net-backend=<configured-name>`.
    #[arg(value_name = "ARGS", allow_hyphen_values = true)]
    pub(crate) args: Vec<String>,
}

#[derive(Debug)]
struct TestRequest {
    flash: Option<bool>,
    lane: Option<String>,
    net_backend: Option<String>,
    passthrough: Vec<String>,
}

impl TestRequest {
    fn parse(args: &[String]) -> Result<Self> {
        let mut request = Self {
            lane: None,
            net_backend: None,
            passthrough: Vec::new(),
            flash: None,
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
                    request.flash = Some(parse_flash(value)?);
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
                    request.flash = Some(parse_flash(value)?);
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

    let (lane_name, lane) = select_lane(test, &request)?;
    let passthrough = passthrough_position(lane)?;
    let mut cmd = match passthrough {
        PassthroughPosition::BeforeSuffix if lane_name == test.default_lane => {
            let flash = request
                .flash
                .unwrap_or_else(|| lane.default_flash.unwrap_or(test.flash.default));
            let backend = request
                .net_backend
                .as_deref()
                .unwrap_or(&test.default_backend);
            let (_, cmd) = nextest_lane_command(&project, flash, backend, &request.passthrough)?;
            cmd
        }
        passthrough => {
            let mut cmd = Command::new(&lane.program);
            cmd.args(&lane.prefix_args);
            let features = features_for(test, lane, &request)?;
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
            cmd
        }
    };

    let status = cmd
        .status()
        .with_context(|| format!("failed to run test lane `{lane_name}`: {}", lane.program))?;
    if !status.success() {
        bail!(
            "test lane `{lane_name}` failed (exit code {:?})",
            status.code()
        );
    }
    Ok(())
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
    let lane_name = request.lane.as_deref().unwrap_or(&config.default_lane);
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

fn features_for(
    config: &TestCommandConfig,
    lane: &TestLaneConfig,
    request: &TestRequest,
) -> Result<BTreeSet<String>> {
    let flash = request
        .flash
        .unwrap_or_else(|| lane.default_flash.unwrap_or(config.flash.default));
    let backend_name = request
        .net_backend
        .clone()
        .unwrap_or_else(|| config.default_backend.clone());
    lane_features(config, lane, flash, &backend_name)
}

pub(crate) fn lane_features(
    config: &TestCommandConfig,
    lane: &TestLaneConfig,
    flash: bool,
    backend_name: &str,
) -> Result<BTreeSet<String>> {
    let mut features = BTreeSet::new();
    features.extend(lane.default_features.iter().cloned());
    if flash {
        features.extend(config.flash.features.iter().cloned());
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
    flash: bool,
    backend: &str,
    extra: &[String],
) -> Result<(Vec<String>, Command)> {
    let test = &project.test;
    validate_config(test)?;
    let lane_name = &test.default_lane;
    let Some(lane) = test.lanes.get(lane_name) else {
        bail!("test.default_lane `{lane_name}` is not defined in test.lanes");
    };
    let features = lane_features(test, lane, flash, backend)?;
    let mut cmd = Command::new(&lane.program);
    cmd.args(&lane.prefix_args);
    if !features.is_empty() {
        cmd.arg(&test.feature_arg)
            .arg(features.iter().cloned().collect::<Vec<_>>().join(","));
    }
    cmd.args(extra);
    cmd.args(&lane.suffix_args);
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

fn parse_flash(value: &str) -> Result<bool> {
    match value {
        "on" | "true" => Ok(true),
        "off" | "false" => Ok(false),
        _ => bail!("unsupported flash mode: {value}"),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::common::project::{
        HealthConfig, LintExcludeConfig, OrphansConfig, PerfConfig, ProjectIdentity, QualityConfig,
        TestFlashConfig, TestNetBackendConfig, WorkspaceScan,
    };

    fn args_of(cmd: &Command) -> Vec<String> {
        cmd.get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
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
                passthrough: String::new(),
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
            project: ProjectIdentity {
                name: "demo".to_owned(),
            },
            health: HealthConfig::default(),
            test: TestCommandConfig {
                lanes,
                net_backends,
                default_lane: "workspace".to_owned(),
                default_backend: "http".to_owned(),
                feature_arg: "--features".to_owned(),
                flash: TestFlashConfig {
                    features: vec!["virtual-time".to_owned()],
                    default: true,
                },
            },
            lint_exclude: LintExcludeConfig::default(),
            workspace_scan: WorkspaceScan::default(),
            orphans: OrphansConfig::default(),
            quality: QualityConfig::default(),
            perf: PerfConfig::default(),
            ext: toml::Table::default(),
        }
    }

    #[test]
    fn lane_features_flash_and_backend() {
        let project = synthetic_project();
        let test = &project.test;
        let lane = &test.lanes[&test.default_lane];

        let feats = lane_features(test, lane, true, "native").expect("features");
        assert!(feats.contains("virtual-time"));
        assert!(feats.contains("demo/native-net"));

        let feats = lane_features(test, lane, false, "http").expect("features");
        assert!(feats.is_empty());
    }

    #[test]
    fn nextest_lane_command_shape() {
        let project = synthetic_project();
        let extra = vec!["--profile".to_owned(), "perf".to_owned()];

        let (features, cmd) =
            nextest_lane_command(&project, true, "http", &extra).expect("nextest command");

        assert_eq!(features, vec!["virtual-time".to_owned()]);
        let args = args_of(&cmd);
        assert_eq!(cmd.get_program().to_string_lossy(), "cargo");
        assert!(args.windows(2).any(|w| w == ["--profile", "perf"]));
        assert!(args.contains(&"--workspace".to_owned()));
        assert_eq!(args.last().map(String::as_str), Some("--locked"));
    }
}

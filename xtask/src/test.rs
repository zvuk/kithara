use std::{collections::BTreeSet, path::Path, process::Command};

use anyhow::{Context, Result, bail};
use clap::Args;
use kithara_xtask_core::common::project::{ProjectConfig, TestCommandConfig, TestLaneConfig};

#[derive(Debug, Args)]
#[command(trailing_var_arg = true)]
pub(crate) struct TestArgs {
    /// Arguments for the configured test command. Recipe-level flags accepted anywhere:
    /// `--lane=<configured-name>`, `--flash=true|false|on|off`, `--no-flash`,
    /// and `--net-backend=<configured-name>`.
    #[arg(value_name = "ARGS", allow_hyphen_values = true)]
    pub(crate) args: Vec<String>,
}

#[derive(Debug)]
struct TestRequest {
    lane: Option<String>,
    net_backend: Option<String>,
    passthrough: Vec<String>,
    flash: Option<bool>,
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
    let mut cmd = Command::new(&lane.program);
    cmd.args(&lane.prefix_args);
    let features = features_for(test, lane, &request)?;
    if !features.is_empty() {
        cmd.arg(&test.feature_arg)
            .arg(features.into_iter().collect::<Vec<_>>().join(","));
    }
    match passthrough_position(lane)? {
        PassthroughPosition::BeforeSuffix => {
            cmd.args(&request.passthrough);
            cmd.args(&lane.suffix_args);
        }
        PassthroughPosition::AfterSuffix => {
            cmd.args(&lane.suffix_args);
            cmd.args(&request.passthrough);
        }
    }

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
    let mut features = BTreeSet::new();
    features.extend(lane.default_features.iter().cloned());
    let flash = request
        .flash
        .unwrap_or_else(|| lane.default_flash.unwrap_or(config.flash.default));
    if flash {
        features.extend(config.flash.features.iter().cloned());
    }
    let backend_name = request
        .net_backend
        .as_ref()
        .unwrap_or(&config.default_backend);
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

use std::{
    collections::{BTreeMap, BTreeSet},
    process::Command,
};

use anyhow::{Context, Result, bail};
use cargo_metadata::MetadataCommand;
use serde::{Deserialize, Serialize};

use super::{
    NextestAction,
    selection::{LaneToggles, lane_features, validate_config},
};
use crate::common::project::{TestCargoOptions, TestCommandConfig, TestRunner};

/// What a caller asks of one lane: the lane, its backend and its toggles.
pub(crate) struct LaneChoice<'a> {
    pub(crate) backend: &'a str,
    pub(crate) lane: &'a str,
    pub(crate) toggles: LaneToggles,
}

/// One lane resolved to everything its command is rendered from.
///
/// A stress manifest records it, so a report compares the runner a campaign
/// ran with against the one the configuration resolves to now.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ResolvedLane {
    pub(crate) env: BTreeMap<String, String>,
    pub(crate) backend: String,
    pub(crate) lane: String,
    pub(crate) cargo: TestCargoOptions,
    pub(crate) runner: TestRunner,
    pub(crate) features: Vec<String>,
}

/// Resolves `choice` against the lane configuration.
pub(crate) fn resolve(test: &TestCommandConfig, choice: &LaneChoice<'_>) -> Result<ResolvedLane> {
    validate_config(test)?;
    let lane = test
        .lanes
        .get(choice.lane)
        .with_context(|| format!("test lane `{}` is not configured", choice.lane))?;
    let features = lane_features(test, lane, choice.toggles, choice.backend)?;
    Ok(ResolvedLane {
        env: lane.env.clone(),
        backend: choice.backend.to_owned(),
        lane: choice.lane.to_owned(),
        cargo: lane.cargo.clone(),
        runner: lane.runner.clone(),
        features: features.into_iter().collect(),
    })
}

impl ResolvedLane {
    /// The lane's command for `action`, with the caller's arguments placed by
    /// three rules. A `cargo test` lane that names its own Cargo profile keeps
    /// it, because `cargo test` refuses a second `--profile`. A caller
    /// filterset narrows the lane's filter instead of joining it, because
    /// nextest unions repeated `-E`. A caller package selection replaces a
    /// workspace selection, so `-p` narrows any workspace lane.
    pub(crate) fn command(&self, action: NextestAction, caller: &[String]) -> Result<Command> {
        let mut command = Command::new(match &self.runner {
            TestRunner::Nextest(_) => "cargo-nextest",
            TestRunner::Cargo(_) => "cargo",
        });
        if matches!(&self.runner, TestRunner::Nextest(_)) && !self.env.contains_key("CARGO") {
            command.env(
                "CARGO",
                which::which(MetadataCommand::new().cargo_command().get_program())
                    .context("resolve Cargo for the nextest lane")?,
            );
        }
        command.envs(&self.env);
        match &self.runner {
            TestRunner::Nextest(nextest) => {
                command.arg("nextest").arg(match action {
                    NextestAction::Run => "run",
                    NextestAction::List => "list",
                });
                command.args(self.cargo_args("--cargo-profile", selects_packages(caller)));
                if let (NextestAction::Run, Some(threads)) = (action, nextest.test_threads) {
                    command.arg("--test-threads").arg(threads.to_string());
                }
                if nextest.ignore_default_filter {
                    command.arg("--ignore-default-filter");
                }
                let mut caller = caller.to_vec();
                if let Some(filter) = &nextest.filter {
                    let (rest, filters) = split_filters(&caller)?;
                    command.arg("-E").arg(intersect(filter, &filters));
                    caller = rest;
                }
                command.args(caller);
            }
            TestRunner::Cargo(cargo) => {
                if action == NextestAction::List {
                    bail!(
                        "test lane `{}` runs `cargo test`, which has no inventory to list",
                        self.lane
                    );
                }
                let (cargo_caller, binary_caller) = split_at_separator(caller);
                command.arg("test");
                command.args(self.cargo_args("--profile", selects_packages(cargo_caller)));
                if self.cargo.profile.is_some() {
                    command.args(without_profile(cargo_caller));
                } else {
                    command.args(cargo_caller);
                }
                if !cargo.name_filters.is_empty() || cargo.no_capture || !binary_caller.is_empty() {
                    command.arg("--");
                    command.args(&cargo.name_filters);
                    if cargo.no_capture {
                        command.arg("--nocapture");
                    }
                    command.args(binary_caller);
                }
            }
        }
        Ok(command)
    }

    /// The lane with its own build, running only the tests it selects that
    /// `filterset` selects too.
    pub(crate) fn filtered(mut self, filterset: String) -> Result<Self> {
        let TestRunner::Nextest(nextest) = &mut self.runner else {
            bail!(
                "test lane `{}` runs `cargo test`, which a filterset cannot narrow",
                self.lane
            );
        };
        nextest.filter = Some(match &nextest.filter {
            Some(filter) => intersect(filter, &[filterset]),
            None => filterset,
        });
        Ok(self)
    }

    /// The cargo arguments the lane's typed options derive. `profile_flag` is
    /// the one flag the runners spell differently: nextest takes the Cargo
    /// profile as `--cargo-profile` and hands it to `cargo test` as
    /// `--profile`. A caller that selects packages drops the workspace
    /// selection.
    pub(super) fn cargo_args(&self, profile_flag: &str, caller_selects: bool) -> Vec<String> {
        let mut args = Vec::new();
        if matches!(&self.runner, TestRunner::Cargo(cargo) if cargo.doc) {
            args.push("--doc".to_owned());
        }
        if self.cargo.workspace && !caller_selects {
            args.push("--workspace".to_owned());
            for package in &self.cargo.exclude {
                args.extend(["--exclude".to_owned(), package.clone()]);
            }
        }
        for package in &self.cargo.packages {
            args.extend(["-p".to_owned(), package.clone()]);
        }
        if let Some(profile) = &self.cargo.profile {
            args.extend([profile_flag.to_owned(), profile.clone()]);
        }
        if self.cargo.lib {
            args.push("--lib".to_owned());
        }
        for test in &self.cargo.tests {
            args.extend(["--test".to_owned(), test.clone()]);
        }
        if !self.features.is_empty() {
            args.extend(["--features".to_owned(), self.features.join(",")]);
        }
        args
    }

    /// A recorded runner has to be one a lane could resolve to.
    pub(crate) fn validate(&self) -> Result<()> {
        for (field, value) in [
            ("lane", self.lane.as_str()),
            ("backend", self.backend.as_str()),
        ] {
            if value.trim().is_empty() {
                bail!("resolved test lane {field} is empty");
            }
        }
        let mut features = BTreeSet::new();
        for feature in &self.features {
            if feature.trim().is_empty() {
                bail!(
                    "resolved test lane `{}` carries an empty feature",
                    self.lane
                );
            }
            if !features.insert(feature) {
                bail!(
                    "resolved test lane `{}` carries feature `{feature}` twice",
                    self.lane
                );
            }
        }
        Ok(())
    }
}

/// `args` without `--profile` and its value.
fn without_profile(args: &[String]) -> Vec<String> {
    let mut kept = Vec::with_capacity(args.len());
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--profile" {
            iter.next();
        } else if !arg.starts_with("--profile=") {
            kept.push(arg.clone());
        }
    }
    kept
}

/// Whether the caller names packages, which replaces a workspace selection.
fn selects_packages(args: &[String]) -> bool {
    args.iter()
        .any(|arg| matches!(arg.as_str(), "-p" | "--package") || arg.starts_with("--package="))
}

/// The caller's cargo arguments, and the ones after `--` for the test binary.
fn split_at_separator(args: &[String]) -> (&[String], &[String]) {
    args.iter()
        .position(|arg| arg == "--")
        .map_or((args, &[][..]), |index| {
            let (cargo, binary) = args.split_at(index);
            (cargo, binary.get(1..).unwrap_or_default())
        })
}

/// The caller's arguments without its filtersets, and those filtersets. A
/// filterset flag with no value after it is refused rather than dropped.
fn split_filters(args: &[String]) -> Result<(Vec<String>, Vec<String>)> {
    const VALUED: [&str; 3] = ["-E", "--filterset", "--filter-expr"];
    const ATTACHED: [&str; 4] = ["--filterset=", "--filter-expr=", "-E=", "-E"];
    let mut rest = Vec::with_capacity(args.len());
    let mut filters = Vec::new();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if VALUED.contains(&arg.as_str()) {
            let filter = iter
                .next()
                .with_context(|| format!("`{arg}` needs a filterset after it"))?;
            filters.push(filter.clone());
        } else if let Some(filter) = ATTACHED.iter().find_map(|prefix| arg.strip_prefix(prefix)) {
            filters.push(filter.to_owned());
        } else {
            rest.push(arg.clone());
        }
    }
    Ok((rest, filters))
}

/// The lane's filter narrowed by the union of the caller's filtersets.
fn intersect(lane: &str, filters: &[String]) -> String {
    if filters.is_empty() {
        return lane.to_owned();
    }
    let union = filters
        .iter()
        .map(|filter| format!("({filter})"))
        .collect::<Vec<_>>()
        .join(" | ");
    format!("({lane}) & ({union})")
}

//! Canonical stress units and their execution selection.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, bail, ensure};

use super::manifest::{PolicySnapshot, StressRunner};
use crate::{
    common::project::{ProjectConfig, StressConfig, StressModeConfig, TestRunner},
    test::{resolve, toggled},
};

/// One body of evidence a run produces: a lane mode on one test lane, or a
/// command mode on its own.
#[derive(Debug)]
pub(super) struct Unit<'a> {
    pub(super) mode_name: &'a str,
    pub(super) mode: &'a StressModeConfig,
    /// The test lane a lane mode runs on; a command mode has none.
    pub(super) lane: Option<&'a str>,
    pub(super) runner: StressRunner,
    pub(super) test_threads: &'a str,
}

impl Unit<'_> {
    /// Where the unit's evidence lands inside the run: `<mode>/<lane>`, or
    /// `<mode>` for a command mode.
    pub(super) fn directory(&self) -> PathBuf {
        let mode = PathBuf::from(self.mode_name);
        self.lane
            .map_or_else(|| mode.clone(), |lane| mode.join(lane))
    }

    /// The unit as the report names it.
    pub(super) fn name(&self) -> String {
        self.lane.map_or_else(
            || self.mode_name.to_owned(),
            |lane| format!("{}/{lane}", self.mode_name),
        )
    }
}

/// The modes this invocation is made of: what was asked for, or what the
/// project says a run is.
pub(super) fn resolve_modes(requested: &[String], config: &StressConfig) -> Result<Vec<String>> {
    let modes = if requested.is_empty() {
        config.default_modes.clone()
    } else {
        requested.to_vec()
    };
    ensure!(!modes.is_empty(), "a run must name at least one mode");
    let mut seen = BTreeSet::new();
    for mode in &modes {
        config.mode(mode)?;
        validate_directory_name("mode", mode)?;
        ensure!(seen.insert(mode), "stress mode `{mode}` is named twice");
    }
    Ok(modes)
}

/// The test lanes this invocation's lane modes run on: the lanes asked for,
/// or else the named tier's, or else the default tier's.
pub(super) fn resolve_lanes(
    tier: Option<&str>,
    requested: &[String],
    project: &ProjectConfig,
) -> Result<Vec<String>> {
    let config = &project.stress;
    let lanes = if requested.is_empty() {
        config
            .tier(tier.unwrap_or(&config.default_tier))?
            .lanes
            .clone()
    } else {
        requested.to_vec()
    };
    let mut seen = BTreeSet::new();
    for lane in &lanes {
        validate_directory_name("lane", lane)?;
        ensure!(seen.insert(lane), "stress lane `{lane}` is named twice");
        if !requested.is_empty() {
            if let Some(reason) = config.not_stressed.get(lane) {
                bail!("stress lane `{lane}` is excluded: {reason}");
            }
            let configured = project
                .test
                .lanes
                .get(lane)
                .with_context(|| format!("stress lane `{lane}` is not configured"))?;
            ensure!(
                matches!(&configured.runner, TestRunner::Nextest(_)),
                "stress lane `{lane}` does not produce Nextest evidence"
            );
        }
    }
    Ok(lanes)
}

/// A mode or a lane names the directory its evidence lands in, so it has to be
/// a plain directory name rather than anything that could climb out of the run.
pub(super) fn validate_directory_name(kind: &str, name: &str) -> Result<()> {
    let mut components = Path::new(name).components();
    let single =
        matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none();
    ensure!(
        single && !name.is_empty(),
        "stress {kind} `{name}` is not usable as a directory name"
    );
    Ok(())
}

/// Resolves each lane mode on every lane and each command mode once, preserving
/// requested order and deduplicating globally by runner and policy.
pub(super) fn units<'a>(
    project: &ProjectConfig,
    config: &'a StressConfig,
    modes: &'a [String],
    lanes: &'a [String],
) -> Result<Vec<Unit<'a>>> {
    let mut seen = Vec::<(StressRunner, PolicySnapshot, &str)>::new();
    let mut units = Vec::new();
    for mode_name in modes {
        let mode = config.mode(mode_name)?;
        let test_threads = mode.test_threads.as_deref().unwrap_or(&config.test_threads);
        let targets = if mode.command.is_empty() {
            lanes.iter().map(|lane| Some(lane.as_str())).collect()
        } else {
            vec![None]
        };
        for lane in targets {
            let runner = unit_runner(project, mode, lane)?;
            let identity = (runner.clone(), policy_snapshot(config, mode), test_threads);
            if seen.contains(&identity) {
                continue;
            }
            seen.push(identity);
            units.push(Unit {
                mode_name,
                mode,
                lane,
                runner,
                test_threads,
            });
        }
    }
    Ok(units)
}

/// Resolves the lane's backend and supported toggles, or records the mode's own
/// command when there is no lane.
pub(super) fn unit_runner(
    project: &ProjectConfig,
    mode: &StressModeConfig,
    lane: Option<&str>,
) -> Result<StressRunner> {
    let Some(lane) = lane else {
        return Ok(StressRunner::Command(mode.command.clone()));
    };
    let choice = toggled(&project.test, lane, mode.flash, mode.no_block, mode.load)?;
    resolve(&project.test, &choice).map(|lane| StressRunner::Lane(Box::new(lane)))
}

/// Partitions canonical units after their runner and policy were deduplicated.
#[derive(Clone, Copy, Debug, serde::Serialize)]
pub(super) struct Shard {
    index: usize,
    count: usize,
}

impl Shard {
    pub(super) fn new(index: Option<usize>, count: Option<usize>) -> Result<Self> {
        let (index, count) = match (index, count) {
            (None, None) => (0, 1),
            (Some(index), Some(count)) => (index, count),
            _ => anyhow::bail!("--shard-index and --shard-count must be supplied together"),
        };
        ensure!(count > 0, "stress shard count must be greater than zero");
        ensure!(
            index < count,
            "stress shard index {index} must be less than count {count}"
        );
        Ok(Self { index, count })
    }

    /// Lane modes keep requested ordinals; command modes share the following
    /// group so one shard reuses their build.
    pub(super) fn partition<'a>(
        &self,
        units: Vec<Unit<'a>>,
        modes: &[String],
        config: &StressConfig,
    ) -> Result<Vec<Unit<'a>>> {
        let mut groups = BTreeMap::new();
        let mut lane_ordinal = 0;
        for name in modes {
            if config.mode(name)?.command.is_empty() {
                groups.insert(name.as_str(), lane_ordinal);
                lane_ordinal += 1;
            }
        }
        for name in modes {
            groups.entry(name.as_str()).or_insert(lane_ordinal);
        }
        Ok(units
            .into_iter()
            .filter(|unit| {
                groups
                    .get(unit.mode_name)
                    .is_some_and(|ordinal| ordinal % self.count == self.index)
            })
            .collect())
    }
}

pub(super) fn policy_snapshot(config: &StressConfig, mode: &StressModeConfig) -> PolicySnapshot {
    PolicySnapshot {
        remove_env: config.environment.remove.clone(),
        set_env: mode.set_env.clone(),
        raw_path_env: mode.raw_path_env.clone(),
        evidence: config.evidence.clone(),
    }
}

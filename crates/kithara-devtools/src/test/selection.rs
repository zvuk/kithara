use std::collections::BTreeSet;

use anyhow::{Context, Result, bail};

use super::{request::TestRequest, resolve::LaneChoice};
use crate::{
    common::project::{TestCommandConfig, TestLaneConfig},
    consts,
};

pub(super) fn validate_config(config: &TestCommandConfig) -> Result<()> {
    if config.default_lane.is_empty() {
        bail!("missing test.default_lane in .config/xtask.toml");
    }
    if config.default_backend.is_empty() {
        bail!("missing test.default_backend in .config/xtask.toml");
    }
    if config.nextest_config.is_empty() {
        bail!("missing test.nextest_config in .config/xtask.toml");
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
    for (name, lane) in &config.lanes {
        for entry in &lane.undeclared_toggles {
            if entry != consts::FLASH_TOGGLE
                && entry != consts::NO_BLOCK_TOGGLE
                && entry != consts::LOAD_TOGGLE
            {
                bail!(
                    "test.lanes.{name}.undeclared_toggles carries `{entry}`; valid toggles are `{FLASH_TOGGLE}`, `{NO_BLOCK_TOGGLE}` and `{LOAD_TOGGLE}`",
                    FLASH_TOGGLE = consts::FLASH_TOGGLE,
                    NO_BLOCK_TOGGLE = consts::NO_BLOCK_TOGGLE,
                    LOAD_TOGGLE = consts::LOAD_TOGGLE
                );
            }
        }
    }
    if !config.net_backends.contains_key(&config.default_backend) {
        bail!(
            "test.default_backend `{}` is not defined in test.net_backends",
            config.default_backend
        );
    }
    for (name, lane) in &config.lanes {
        if lane.cargo.workspace != lane.cargo.packages.is_empty() {
            bail!("test.lanes.{name}.cargo needs exactly one of `workspace = true` or `packages`");
        }
        if !lane.cargo.workspace && !lane.cargo.exclude.is_empty() {
            bail!("test.lanes.{name}.cargo.exclude needs `workspace = true`");
        }
        if let Some(backend) = &lane.default_backend
            && !config.net_backends.contains_key(backend)
        {
            bail!(
                "test.lanes.{name}.default_backend `{backend}` is not configured under test.net_backends"
            );
        }
    }
    let mut named = BTreeSet::new();
    for flake in &config.known_flakes {
        if flake.test.is_empty() {
            bail!("test.known_flakes entry names no test");
        }
        if flake.issue.is_empty() {
            bail!(
                "test.known_flakes entry `{}` names no issue: an entry without an owner is a flake nobody removes",
                flake.test
            );
        }
        if !named.insert(flake.test.as_str()) {
            bail!("test.known_flakes names `{}` twice", flake.test);
        }
    }
    Ok(())
}

pub(super) fn select_lane<'a>(
    config: &'a TestCommandConfig,
    request: &'a TestRequest,
) -> Result<&'a str> {
    let explicit_lane = match request.lanes.as_slice() {
        [] => None,
        [lane] => Some(lane.as_str()),
        _ => bail!("--lane names one lane; run each lane on its own"),
    };
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
    if !config.lanes.contains_key(lane_name) {
        let valid = config
            .lanes
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        bail!("unsupported test lane `{lane_name}`; configured values: {valid}");
    }
    Ok(lane_name)
}

#[derive(Clone, Copy)]
pub(crate) struct LaneToggles {
    pub(crate) flash: bool,
    pub(crate) no_block: bool,
    pub(crate) load: bool,
}

/// Resolve one toggle for a lane.
///
/// A lane lists a toggle in `undeclared_toggles` when none of its packages
/// declares that feature: the tools have no `no-block`, the UI crates have no
/// `flash`. Cargo applies an unqualified feature to every selected package and
/// fails the run when none of them declares it, so a run-wide request — the
/// gate asks every lane it runs for the detector — must leave such a lane
/// alone. A lane that merely defaults a toggle off still honours the request.
pub(super) fn toggle(
    name: &str,
    requested: Option<bool>,
    lane_default: Option<bool>,
    lane: &TestLaneConfig,
    default: bool,
) -> bool {
    if lane.undeclared_toggles.iter().any(|entry| entry == name) {
        return false;
    }
    requested.unwrap_or_else(|| lane_default.unwrap_or(default))
}

fn lane_toggles(
    config: &TestCommandConfig,
    lane: &TestLaneConfig,
    flash: Option<bool>,
    no_block: Option<bool>,
    load: Option<bool>,
) -> LaneToggles {
    LaneToggles {
        flash: toggle(
            consts::FLASH_TOGGLE,
            flash,
            lane.default_flash,
            lane,
            config.flash.enabled,
        ),
        no_block: toggle(
            consts::NO_BLOCK_TOGGLE,
            no_block,
            lane.default_no_block,
            lane,
            config.no_block.enabled,
        ),
        load: toggle(
            consts::LOAD_TOGGLE,
            load,
            lane.default_load,
            lane,
            config.load.enabled,
        ),
    }
}

pub(super) fn lane_features(
    config: &TestCommandConfig,
    lane: &TestLaneConfig,
    toggles: LaneToggles,
    backend_name: &str,
) -> Result<BTreeSet<String>> {
    let mut features = BTreeSet::new();
    features.extend(config.features.iter().cloned());
    features.extend(lane.default_features.iter().cloned());
    if toggles.flash {
        features.extend(config.flash.items.iter().cloned());
    }
    if toggles.no_block {
        features.extend(config.no_block.items.iter().cloned());
    }
    if toggles.load {
        features.extend(config.load.items.iter().cloned());
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

/// What a lane runs with when `request` asks for it, or with no request:
/// the backend and toggles the request names, else the lane's defaults,
/// else the project's.
pub(super) fn requested<'a>(
    test: &'a TestCommandConfig,
    lane_name: &'a str,
    request: Option<&'a TestRequest>,
) -> Result<LaneChoice<'a>> {
    choice(
        test,
        lane_name,
        request.and_then(|request| request.net_backend.as_deref()),
        request.and_then(|request| request.flash),
        request.and_then(|request| request.no_block),
        request.and_then(|request| request.load),
    )
}

/// What a lane runs with when a caller asks only for its toggles: the lane's
/// own backend, and the same toggle rule a request follows, so a toggle the
/// lane does not declare stays off and an unset one keeps the lane's default.
pub(crate) fn toggled<'a>(
    test: &'a TestCommandConfig,
    lane_name: &'a str,
    flash: Option<bool>,
    no_block: Option<bool>,
    load: Option<bool>,
) -> Result<LaneChoice<'a>> {
    choice(test, lane_name, None, flash, no_block, load)
}

fn choice<'a>(
    test: &'a TestCommandConfig,
    lane_name: &'a str,
    backend: Option<&'a str>,
    flash: Option<bool>,
    no_block: Option<bool>,
    load: Option<bool>,
) -> Result<LaneChoice<'a>> {
    let lane = test
        .lanes
        .get(lane_name)
        .with_context(|| format!("test lane `{lane_name}` is not configured"))?;
    Ok(LaneChoice {
        backend: backend
            .or(lane.default_backend.as_deref())
            .unwrap_or(&test.default_backend),
        lane: lane_name,
        toggles: lane_toggles(test, lane, flash, no_block, load),
    })
}

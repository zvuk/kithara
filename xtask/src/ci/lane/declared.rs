use std::{env, path::PathBuf, time::Instant};

use anyhow::{Context, Result, bail};
use kithara_devtools::common::tools::ToolsConfig;
use toml::Value;
use tracing::warn;

use crate::{
    ci::{
        cache::snapshot, config::CiPins, environment::CacheTrust, process::Process,
        run::PipelineKind,
    },
    config::{CiLaneConfig, CiLanePin},
    consts,
};

/// Run a lane the way `.config/xtask.toml` declares it: the pipeline kinds it
/// declines, the platform it refuses to run anywhere but on, the tools it needs,
/// the versions those tools have to report, then its commands in order.
///
/// A lane whose whole content is this needs no Rust of its own; the ones that
/// keep a function are the ones that do something a parameter cannot say.
pub(crate) fn run(
    process: &Process,
    lane: &CiLaneConfig,
    pins: &CiPins,
    tools: &ToolsConfig,
    kind: PipelineKind,
) -> Result<()> {
    let kind = kind_name(kind);
    if let Some(reason) = lane.kinds_refused.get(&kind) {
        bail!("{reason}");
    }
    if !lane.os.is_empty() {
        process.require_os(&lane.os, &lane.label)?;
    }
    if !lane.tools.is_empty() {
        let required: Vec<&str> = lane.tools.iter().map(|role| tools.program(role)).collect();
        process.require_tools(&required)?;
    }
    if !lane.left_behind.is_empty() {
        process.require_left_behind(&lane.left_behind, &lane.left_behind_by)?;
    }
    for check in &lane.pinned {
        require_pinned_version(process, check, pins, tools)?;
    }
    let target_snapshot_to_publish = if process.is_recording() {
        None
    } else {
        lane.target_snapshot
            .as_deref()
            .map(|key| restore_target_layer(process, tools, key))
            .transpose()?
    };
    if !process.is_recording() {
        restore_source_layer(process, tools);
    }
    for step in &lane.steps {
        let role = step.program.as_deref().unwrap_or(&lane.program);
        let mut command = if role == consts::SELF_PROGRAM {
            process.command(&env::current_exe().context("locating the running xtask executable")?)
        } else {
            process.command(tools.program(role))
        };
        let args = step.args_by_kind.get(&kind).unwrap_or(&step.args);
        for arg in args {
            command.arg(resolve(arg, process, pins)?);
        }
        for (key, value) in &step.env {
            command.env(key, resolve(value, process, pins)?);
        }
        process.run_command(&mut command, &step.label)?;
    }
    if !process.is_recording() && lane.publishes_sources {
        publish_source_layer(process, tools, &kind)?;
    }
    if let Some(fingerprint) = target_snapshot_to_publish.flatten() {
        let mc = process.resolve_program(tools.program("mc"))?;
        let started = Instant::now();
        let published = snapshot::publish_for_lane(&process.target_dir(), &fingerprint, &mc);
        let took = format!("{:.1} s", started.elapsed().as_secs_f64());
        match published {
            Ok(()) => process.note_cache(
                "target snapshot",
                "published",
                format!("{fingerprint} in {took}"),
            ),
            Err(error) => {
                process.note_cache("target snapshot", "publish failed", format!("{error}"));
                warn!(%error, %fingerprint, "could not publish optional target snapshot");
            }
        }
    }
    Ok(())
}

/// Put the lane's compiled artifacts in place, and say what that cost.
///
/// A fingerprint comes back when nothing was restored: that is the lane which
/// has to publish its own artifacts once the steps pass.
fn restore_target_layer(
    process: &Process,
    tools: &ToolsConfig,
    key: &str,
) -> Result<Option<String>> {
    let mc = process.resolve_program(tools.program("mc"))?;
    let cargo_home = process
        .environment_path("CARGO_HOME")
        .or_else(|| {
            process
                .environment_path("HOME")
                .map(|home| home.join(".cargo"))
        })
        .context("prepared CI environment has no CARGO_HOME")?;
    let started = Instant::now();
    let outcome =
        snapshot::restore_for_lane(key, &process.target_dir(), process.root(), &cargo_home, &mc);
    let took = format!("{:.1} s", started.elapsed().as_secs_f64());
    match &outcome {
        Ok(None) => {
            process.note_cache(
                "target snapshot",
                "restored",
                format!("{key} unpacked in {took}"),
            );
        }
        Ok(Some(fingerprint)) => {
            process.note_cache(
                "target snapshot",
                "miss",
                format!(
                    "no object for {fingerprint}; this lane will publish one (asked in {took})"
                ),
            );
        }
        Err(error) => {
            process.note_cache("target snapshot", "unavailable", format!("{error}"));
        }
    }
    outcome
}

fn kind_name(kind: PipelineKind) -> String {
    kind.name().to_owned()
}

/// Put the dependency sources in place before the lane's first Cargo command.
///
/// A failed restore stays non-fatal: the layer is an accelerator, and a lane
/// that cannot reach the cache must still be able to fetch and run. What it no
/// longer does is report every outcome as the same warning. A lane whose
/// `Cargo.lock` has no object yet is the ordinary case and says so; only a
/// lane that could not ask warns.
fn restore_source_layer(process: &Process, tools: &ToolsConfig) {
    let Some((cargo_home, mc)) = source_layer_access(process, tools) else {
        return;
    };
    let started = Instant::now();
    let outcome = snapshot::restore_sources(process.root(), &cargo_home, &mc);
    let took = format!("{:.1} s", started.elapsed().as_secs_f64());
    match outcome {
        Ok(snapshot::Restored::AlreadyPresent) => {
            process.note_cache(
                "dependency sources",
                "reused",
                format!("already in place for this lock file, checked in {took}"),
            );
        }
        Ok(snapshot::Restored::Absent) => {
            process.note_cache(
                "dependency sources",
                "miss",
                format!("no layer for this lock file yet; the lane will fetch (asked in {took})"),
            );
        }
        Ok(snapshot::Restored::Layer(object)) => {
            process.note_cache(
                "dependency sources",
                "restored",
                format!("{object} in {took}"),
            );
        }
        Err(error) => {
            process.note_cache("dependency sources", "unavailable", format!("{error}"));
            warn!(%error, "could not restore the dependency sources");
        }
    }
}

/// Record what the lane ended up with, once its steps have fetched whatever
/// the restored layer did not carry. Only the default branch in the trusted
/// scope publishes, because that scope is the only one the bucket policy lets
/// write.
///
/// A publisher's failure fails the lane. The defect this repairs is that it did
/// not: the publish was refused on every run for weeks and the lane stayed
/// green, so the layer read as a working cache that happened to always miss.
fn publish_source_layer(process: &Process, tools: &ToolsConfig, kind: &str) -> Result<()> {
    if !is_source_publisher(kind, CacheTrust::from_environment()?) {
        return Ok(());
    }
    let Some((cargo_home, mc)) = source_layer_access(process, tools) else {
        return Ok(());
    };
    let started = Instant::now();
    let published = snapshot::publish_sources(process.root(), &cargo_home, &mc)
        .context("publish the dependency sources");
    let took = format!("{:.1} s", started.elapsed().as_secs_f64());
    match &published {
        Ok(()) => {
            process.note_cache("dependency sources", "published", format!("in {took}"));
        }
        Err(error) => {
            process.note_cache("dependency sources", "publish failed", format!("{error}"));
        }
    }
    published
}

/// A default-branch run in another scope holds that scope's keys: a fork's
/// `main` runs as `review`, and its publish was refused on every push.
fn is_source_publisher(kind: &str, trust: CacheTrust) -> bool {
    kind == PipelineKind::Main.name() && trust == CacheTrust::Trusted
}

fn source_layer_access(process: &Process, tools: &ToolsConfig) -> Option<(PathBuf, PathBuf)> {
    let cargo_home = process.environment_path("CARGO_HOME")?;
    match process.resolve_program(tools.program("mc")) {
        Ok(mc) => Some((cargo_home, mc)),
        Err(error) => {
            warn!(%error, "no cache client; the lane carries its own sources");
            None
        }
    }
}

/// Fill in the things a lane cannot spell for itself: where the checkout is,
/// where its leased build cache lives, and what a reviewed pin currently
/// holds.
fn resolve(value: &str, process: &Process, pins: &CiPins) -> Result<String> {
    let mut filled = value
        .replace(
            consts::ROOT_PLACEHOLDER,
            &process.root().display().to_string(),
        )
        .replace(
            consts::TARGET_PLACEHOLDER,
            &process.target_dir().display().to_string(),
        );
    while let Some(start) = filled.find(consts::PIN_PREFIX) {
        let tail = &filled[start + consts::PIN_PREFIX.len()..];
        let end = tail.find('}').with_context(|| {
            format!(
                "{PIN_PREFIX} is unclosed in `{value}`",
                PIN_PREFIX = consts::PIN_PREFIX
            )
        })?;
        let replacement = pin(pins, &tail[..end])?;
        filled.replace_range(start..=start + consts::PIN_PREFIX.len() + end, &replacement);
    }
    Ok(filled)
}

fn require_pinned_version(
    process: &Process,
    check: &CiLanePin,
    pins: &CiPins,
    tools: &ToolsConfig,
) -> Result<()> {
    let expected = pin(pins, &check.pin)?;
    let args: Vec<&str> = check.args.iter().map(String::as_str).collect();
    let label = format!("read {} version", check.tool);
    let actual = process.capture(tools.program(&check.tool), &args, &label)?;
    let Some(prefix) = check.line_prefix.as_deref() else {
        if !actual.split_whitespace().any(|part| part == expected) {
            bail!(
                "{} version mismatch: expected {expected}, got {actual}",
                check.tool
            );
        }
        return Ok(());
    };
    let reported = actual
        .lines()
        .next()
        .and_then(|line| line.strip_prefix(prefix))
        .with_context(|| format!("{} did not report a version", check.tool))?;
    if reported != expected {
        bail!("{prefix}{expected} is required, found {reported}");
    }
    Ok(())
}

/// Pins are reviewed as data, so a lane names the one it wants rather than
/// reaching for a field: a new pin costs a line in `.config/ci-pins.toml`.
fn pin(pins: &CiPins, key: &str) -> Result<String> {
    let table = Value::try_from(pins)?;
    match table.get(key) {
        Some(Value::String(value)) => Ok(value.clone()),
        Some(other) => bail!(
            "pin {key} is {} rather than a version string",
            other.type_str()
        ),
        None => bail!("{key} is not a pin in .config/ci-pins.toml"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_trusted_default_branch_publishes_the_source_layer() {
        let main = PipelineKind::Main.name();

        assert!(is_source_publisher(main, CacheTrust::Trusted));
        assert!(!is_source_publisher(main, CacheTrust::Review));
        assert!(!is_source_publisher(main, CacheTrust::Quarantine));
        assert!(!is_source_publisher(
            PipelineKind::Branch.name(),
            CacheTrust::Trusted
        ));
    }
}

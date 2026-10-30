use std::{
    collections::BTreeMap,
    env,
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    time::Instant,
};

use anyhow::{Context, Result, bail};
use kithara_devtools::{
    common::{project::ProjectConfig, tools::ToolsConfig},
    test::selects_any_lane,
};
use toml::Value;
use tracing::warn;

use super::nextest;
use crate::{
    ci::{cache::snapshot, config::CiPins, process::Process, run::PipelineKind},
    config::{CiLaneConfig, CiLanePin, CiLaneStep},
    consts,
};

/// Run a lane the way `.config/xtask.toml` declares it: the pipeline kinds it
/// declines, the platform it refuses to run anywhere but on, the tools it needs,
/// the versions those tools have to report, then its commands in order. A step
/// that checks its rebuild is repeated building only.
///
/// A lane whose whole content is this needs no Rust of its own; the ones that
/// keep a function are the ones that do something a parameter cannot say.
pub(crate) fn run(
    process: &Process,
    lane: &CiLaneConfig,
    pins: &CiPins,
    tools: &ToolsConfig,
    kind: PipelineKind,
    narrow: Option<&str>,
) -> Result<()> {
    let kind = kind_name(kind);
    admit(lane, &kind, narrow)?;
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
    let nextest_bin = nextest::prepare(process, pins, tools.program("cargo"))?;
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
        let program = if role == consts::SELF_PROGRAM {
            env::current_exe()
                .context("locating the running xtask executable")?
                .into_os_string()
        } else {
            OsString::from(tools.program(role))
        };
        let declared = step.args_in(&kind);
        let mut args = declared
            .iter()
            .map(|arg| resolve(arg, process, pins))
            .collect::<Result<Vec<_>>>()?;
        if let Some(filterset) = narrow
            && let Some(request) = step.suite_request(lane, declared)
        {
            args.insert(
                declared.len() - request.len(),
                format!("--narrow={filterset}"),
            );
        }
        let vars = step_vars(step, process, pins)?;
        let mut command = process.command(&program);
        command.args(&args).envs(&vars);
        if let Some(bin) = &nextest_bin {
            nextest::prepend(&mut command, bin)?;
        }
        process.run_command(&mut command, &step.label)?;
        if step.rebuild_check {
            rebuild_check(
                process,
                &program,
                &args,
                &vars,
                &step.label,
                nextest_bin.as_deref(),
            )?;
        }
    }
    if !process.is_recording() && lane.publishes_sources {
        publish_source_layer(process, tools, &kind)?;
    }
    if let Some(fingerprint) = target_snapshot_to_publish.flatten() {
        let rc = process.resolve_program(tools.program("rc"))?;
        let started = Instant::now();
        let published = snapshot::publish_for_lane(&process.target_dir(), &fingerprint, &rc);
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

/// Whether the lane has anything to run in `kind`: any step but a suite that
/// selects no test lane on this branch. A lane with nothing to run has nothing
/// to build either.
pub(crate) fn runs_anything(
    lane: &CiLaneConfig,
    kind: PipelineKind,
    narrow: Option<&str>,
    root: &Path,
    project: &ProjectConfig,
    base: &str,
) -> Result<bool> {
    let kind = kind_name(kind);
    admit(lane, &kind, narrow)?;
    for step in &lane.steps {
        let Some(request) = step.suite_request(lane, step.args_in(&kind)) else {
            return Ok(true);
        };
        if selects_any_lane(root, project, request, base)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Refuses what the lane declines before anything runs: a pipeline kind it
/// declines, or a filterset it has no test suite to narrow.
fn admit(lane: &CiLaneConfig, kind: &str, narrow: Option<&str>) -> Result<()> {
    if let Some(reason) = lane.kinds_refused.get(kind) {
        bail!("{reason}");
    }
    let has_suite = lane
        .steps
        .iter()
        .any(|step| step.suite_request(lane, step.args_in(kind)).is_some());
    if narrow.is_some() && !has_suite {
        bail!(
            "lane `{}` runs no test suite a filterset could narrow",
            lane.label
        );
    }
    Ok(())
}

/// Asks cargo what the next job of this commit would build: repeats the step
/// building only, in the same build directory, with cargo saying why it builds
/// each unit. The timings report is left out: a build that compiles nothing
/// would still replace the suite's own.
fn rebuild_check(
    process: &Process,
    program: &OsStr,
    args: &[String],
    vars: &BTreeMap<String, String>,
    label: &str,
    nextest_bin: Option<&Path>,
) -> Result<()> {
    let repeat = args
        .iter()
        .map(String::as_str)
        .filter(|arg| !arg.starts_with("--timings"))
        .chain(consts::REBUILD_CHECK_ARGS);
    let label = format!("{label} rebuild check");
    let mut command = process.command(program);
    command.args(repeat).envs(vars);
    if let Some(bin) = nextest_bin {
        nextest::prepend(&mut command, bin)?;
    }
    let transcript = process.transcript(&mut command, &label)?;
    let rebuilt = rebuilt(&transcript);
    if !rebuilt.is_empty() {
        bail!(
            "{label}: the next job of this commit would build again:\n{}",
            rebuilt.join("\n")
        );
    }
    Ok(())
}

/// Cargo's status lines for units it would build: `Dirty` says why, and
/// `Compiling` also names a unit it never built.
fn rebuilt(transcript: &str) -> Vec<&str> {
    transcript
        .lines()
        .map(str::trim_start)
        .filter(|line| line.starts_with("Dirty ") || line.starts_with("Compiling "))
        .collect()
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
    let rc = process.resolve_program(tools.program("rc"))?;
    let cargo_home = process
        .cargo_home()
        .context("prepared CI environment has no CARGO_HOME")?;
    let started = Instant::now();
    let outcome =
        snapshot::restore_for_lane(key, &process.target_dir(), process.root(), &cargo_home, &rc);
    let took = format!("{:.1} s", started.elapsed().as_secs_f64());
    match &outcome {
        Ok(None) => {
            process.note_cache(
                "target snapshot",
                "reused",
                format!("{key} ready in {took}"),
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
    let Some((cargo_home, rc)) = source_layer_access(process, tools) else {
        return;
    };
    let started = Instant::now();
    let outcome = snapshot::restore_sources(process.root(), &cargo_home, &rc);
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
/// the restored layer did not carry. Only the default branch publishes, into
/// its own scope's bucket, so a branch never changes what its scope reads.
///
/// A publisher's failure fails the lane. The defect this repairs is that it did
/// not: the publish was refused on every run for weeks and the lane stayed
/// green, so the layer read as a working cache that happened to always miss.
fn publish_source_layer(process: &Process, tools: &ToolsConfig, kind: &str) -> Result<()> {
    if !is_source_publisher(kind) {
        return Ok(());
    }
    let Some((cargo_home, rc)) = source_layer_access(process, tools) else {
        return Ok(());
    };
    let started = Instant::now();
    let published = snapshot::publish_sources(process.root(), &cargo_home, &rc)
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

fn is_source_publisher(kind: &str) -> bool {
    kind == PipelineKind::Main.name()
}

fn source_layer_access(process: &Process, tools: &ToolsConfig) -> Option<(PathBuf, PathBuf)> {
    let cargo_home = process.cargo_home()?;
    match process.resolve_program(tools.program("rc")) {
        Ok(rc) => Some((cargo_home, rc)),
        Err(error) => {
            warn!(%error, "no cache client; the lane carries its own sources");
            None
        }
    }
}

/// Fill in the things a lane cannot spell for itself: where the checkout is
/// and what a reviewed pin currently holds.
fn resolve(value: &str, process: &Process, pins: &CiPins) -> Result<String> {
    let mut filled = value.replace(
        consts::ROOT_PLACEHOLDER,
        &process.root().display().to_string(),
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

/// The step's own variables, resolved against the checkout and the pins.
fn step_vars(
    step: &CiLaneStep,
    process: &Process,
    pins: &CiPins,
) -> Result<BTreeMap<String, String>> {
    step.env
        .iter()
        .map(|(key, value)| Ok((key.clone(), resolve(value, process, pins)?)))
        .collect()
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
    use crate::ci::{
        config::fixture,
        environment::CacheTrust,
        process::{Recording, Step},
    };

    /// A lane whose one step runs the suite and leaves its hang dumps under
    /// the checkout.
    fn suite_lane() -> CiLaneConfig {
        CiLaneConfig {
            label: "fixture".to_owned(),
            program: "just".to_owned(),
            steps: vec![CiLaneStep {
                args: vec!["test".to_owned(), "run".to_owned()],
                label: "suite".to_owned(),
                env: BTreeMap::from([(
                    "KITHARA_HANG_DUMP_DIR".to_owned(),
                    "{root}/target/hang".to_owned(),
                )]),
                ..CiLaneStep::default()
            }],
            ..CiLaneConfig::default()
        }
    }

    /// The steps a lane asks for, recorded against a checkout at `/checkout`.
    fn recorded(lane: &CiLaneConfig) -> Vec<Step> {
        recorded_narrowed(lane, None).unwrap()
    }

    /// The steps a lane asks for when its suite is narrowed to `narrow`.
    fn recorded_narrowed(lane: &CiLaneConfig, narrow: Option<&str>) -> Result<Vec<Step>> {
        let process = Process::recording(Path::new("/checkout"), Recording::default());
        run(
            &process,
            lane,
            &fixture().pins,
            &ToolsConfig::default(),
            PipelineKind::Branch,
            narrow,
        )?;
        Ok(process.recorded().unwrap().steps().to_vec())
    }

    /// A step that is not the suite, run beside it.
    fn formatting_step() -> CiLaneStep {
        CiLaneStep {
            args: vec!["fmt".to_owned()],
            label: "fmt".to_owned(),
            program: Some("cargo".to_owned()),
            ..CiLaneStep::default()
        }
    }

    /// A narrowed lane hands the filterset to the test command, which owns how
    /// it composes with the lane's own, and its rebuild check asks about the
    /// same build. A step that runs no suite is not narrowed.
    #[test]
    fn a_narrowed_lane_hands_the_filterset_to_its_suite() {
        let mut lane = suite_lane();
        lane.steps[0].args.push("--timings".to_owned());
        lane.steps[0].rebuild_check = true;
        lane.steps.push(formatting_step());

        let steps = recorded_narrowed(&lane, Some("test(seek)")).unwrap();

        let args: Vec<_> = steps.iter().map(|step| step.args.join(" ")).collect();
        assert_eq!(
            args,
            [
                "test run --narrow=test(seek) --timings",
                "test run --narrow=test(seek) --no-run --cargo-verbose --color never",
                "fmt",
            ]
        );
    }

    /// A lane with no suite to narrow refuses the filterset before it runs
    /// anything, rather than run whole what was asked to run narrowed.
    #[test]
    fn a_lane_without_a_suite_refuses_a_filterset() {
        let lane = CiLaneConfig {
            label: "fixture".to_owned(),
            program: "just".to_owned(),
            steps: vec![formatting_step()],
            ..CiLaneConfig::default()
        };

        let error = recorded_narrowed(&lane, Some("test(seek)")).expect_err("nothing to narrow");

        assert!(
            error.to_string().contains("runs no test suite"),
            "{error:#}"
        );
    }

    /// A lane's build follows one checkout, so cargo judges it by mtime on
    /// whatever toolchain the step names: the step builds with its own
    /// variables and nothing else.
    #[test]
    fn a_step_builds_with_its_own_variables_alone() {
        let steps = recorded(&suite_lane());

        assert_eq!(
            steps[0].env,
            BTreeMap::from([(
                "KITHARA_HANG_DUMP_DIR".to_owned(),
                "/checkout/target/hang".to_owned()
            )])
        );
    }

    /// The check repeats its step with the same variables, building only, with
    /// cargo saying in plain text why it builds each unit, and without the
    /// timings report a build that compiles nothing would still rewrite.
    #[test]
    fn a_rebuild_check_repeats_its_step_building_only() {
        let mut lane = suite_lane();
        lane.steps[0].args.push("--timings".to_owned());
        lane.steps[0].rebuild_check = true;

        let steps = recorded(&lane);

        let [suite, check] = steps.as_slice() else {
            panic!("the suite and its check: {steps:?}")
        };
        assert_eq!(check.label, "suite rebuild check");
        assert_eq!(
            check.args,
            [
                "test",
                "run",
                "--no-run",
                "--cargo-verbose",
                "--color",
                "never"
            ]
        );
        assert_eq!(check.env, suite.env);
    }

    #[test]
    fn a_rebuild_is_read_from_cargos_status_lines() {
        let transcript = "       Fresh serde v1.0.0\n       Dirty probe v0.0.0 (/w): the file `src/lib.rs` has changed\n   Compiling probe v0.0.0 (/w)\n     Running `rustc --crate-name probe`\nwarning: Compiling is mentioned, not reported\n";

        assert_eq!(
            rebuilt(transcript),
            [
                "Dirty probe v0.0.0 (/w): the file `src/lib.rs` has changed",
                "Compiling probe v0.0.0 (/w)",
            ]
        );
    }

    #[cfg(unix)]
    fn nextest_fixture() -> (tempfile::TempDir, Process, CiPins, PathBuf) {
        use crate::testing::{install_copy, install_script};

        let directory = tempfile::tempdir().expect("isolated nextest lane");
        let root = directory.path();
        let pins = fixture().pins;
        let version = pins
            .cargo_tool_version("cargo-nextest")
            .expect("nextest pin");
        let cargo_home = root.join("cargo-home");
        let executable = cargo_home
            .join("kithara-nextest")
            .join(CacheTrust::from_environment().expect("CI trust").as_str())
            .join(format!("{}-{}", env::consts::OS, env::consts::ARCH))
            .join(version)
            .join("bin/cargo-nextest");
        std::fs::create_dir_all(executable.parent().expect("binary directory"))
            .expect("create pinned tool cache");
        let inherited = Process::new(root, BTreeMap::new());
        let installed = inherited
            .resolve_program("cargo-nextest")
            .expect("installed nextest");
        install_copy(&installed, &executable);
        let obsolete = root.join("obsolete/bin");
        std::fs::create_dir_all(&obsolete).expect("obsolete PATH directory");
        let shadow = obsolete.join("cargo-nextest");
        install_script(
            &shadow,
            "#!/bin/sh\nprintf '%s\\n' 'cargo-nextest 0.9.140'\n",
        );
        let stale = cargo_home.join("bin/cargo-nextest");
        std::fs::create_dir_all(stale.parent().expect("Cargo-home binary directory"))
            .expect("create stale Cargo-home tools");
        install_copy(&shadow, &stale);
        let path = env::join_paths(
            std::iter::once(obsolete).chain(env::split_paths(&env::var_os("PATH").expect("PATH"))),
        )
        .expect("obsolete inherited PATH");
        let process = Process::new(
            root,
            BTreeMap::from([
                (OsString::from("CARGO_HOME"), cargo_home.into_os_string()),
                (OsString::from("PATH"), path),
            ]),
        );
        (directory, process, pins, executable)
    }

    #[cfg(unix)]
    #[test]
    fn a_lane_selects_real_pinned_nextest_over_an_obsolete_step_path() {
        use kithara_devtools::verdict::NotClean;

        let (directory, process, pins, _) = nextest_fixture();
        let root = directory.path();
        let fixture = crate::ci::config::workspace_root()
            .join("crates/kithara-devtools/tests/fixtures/nextest-stress");
        for file in ["Cargo.toml", "stress.rs"] {
            std::fs::copy(fixture.join(file), root.join(file)).expect("copy stress contract");
        }
        std::fs::write(
            root.join("nextest.toml"),
            "[profile.default.junit]\npath = \"junit.xml\"\n",
        )
        .expect("isolated nextest configuration");
        let lane = CiLaneConfig {
            label: "pinned nextest fixture".to_owned(),
            program: "cargo-nextest".to_owned(),
            steps: vec![CiLaneStep {
                args: [
                    "nextest",
                    "run",
                    "--offline",
                    "--profile",
                    "default",
                    "--stress-count",
                    "2",
                    "--no-fail-fast",
                    "--retries",
                    "0",
                    "--color",
                    "never",
                    "--user-config-file",
                    "none",
                    "--config-file",
                    "nextest.toml",
                ]
                .map(str::to_owned)
                .to_vec(),
                label: "stress verdict".to_owned(),
                env: BTreeMap::from([
                    (
                        "PATH".to_owned(),
                        process
                            .environment_path("PATH")
                            .expect("PATH")
                            .to_string_lossy()
                            .into_owned(),
                    ),
                    (
                        "CARGO_TARGET_DIR".to_owned(),
                        root.join("build").display().to_string(),
                    ),
                    ("RUSTFLAGS".to_owned(), String::new()),
                    ("CARGO_ENCODED_RUSTFLAGS".to_owned(), String::new()),
                ]),
                ..CiLaneStep::default()
            }],
            ..CiLaneConfig::default()
        };
        let error = run(
            &process,
            &lane,
            &pins,
            &ToolsConfig::default(),
            PipelineKind::Branch,
            None,
        )
        .expect_err("an earlier stress failure fails the lane");
        assert_eq!(NotClean::report(&error), 100, "{error:#}");
        let xml = std::fs::read_to_string(root.join("target/nextest/default/junit.xml"))
            .expect("real nextest wrote both iterations");
        let cases = kithara_devtools::junit::parse_junit(&xml).expect("actual nextest JUnit");
        let mut iterations = cases
            .iter()
            .map(|case| (case.iteration, case.failed, case.flaky))
            .collect::<Vec<_>>();
        iterations.sort();
        assert_eq!(
            iterations,
            [(Some(0), true, false), (Some(1), false, false)],
            "{xml}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_lane_rejects_a_corrupt_pinned_cache_before_running_steps() {
        let (directory, process, pins, executable) = nextest_fixture();
        let corrupt = "#!/bin/sh\nprintf '%s\\n' 'cargo-nextest 0.9.140'\n";
        crate::testing::install_script(&executable, corrupt);
        let marker = directory.path().join("step-ran");
        let lane = CiLaneConfig {
            label: "corrupt cache fixture".to_owned(),
            program: "touch".to_owned(),
            steps: vec![CiLaneStep {
                args: vec![marker.display().to_string()],
                label: "must not execute".to_owned(),
                ..CiLaneStep::default()
            }],
            ..CiLaneConfig::default()
        };
        let error = run(
            &process,
            &lane,
            &pins,
            &ToolsConfig::default(),
            PipelineKind::Branch,
            None,
        )
        .expect_err("a mismatched cached version fails explicitly");
        assert!(error.to_string().contains("version mismatch"), "{error:#}");
        assert!(!marker.exists(), "no lane step runs with a corrupt cache");
        assert_eq!(
            std::fs::read(executable).expect("unchanged corrupt artifact"),
            corrupt.as_bytes()
        );
    }

    #[cfg(unix)]
    #[test]
    fn nextest_install_uses_process_scratch_and_releases_build_and_lock() {
        use std::fs;

        use kithara_devtools::lock::FileLock;

        use crate::testing::install_script;

        let (directory, inherited, pins, source) = nextest_fixture();
        let root = directory.path();
        let scratch = root.join("scratch");
        let engine_target = root.join("engine-target");
        fs::create_dir_all(&scratch).expect("process-only scratch");
        fs::create_dir_all(&engine_target).expect("isolated engine target");
        let current_trust = CacheTrust::from_environment().expect("inherited trust");
        let trust = if current_trust.as_str() == "trusted" {
            "quarantine"
        } else {
            "trusted"
        };
        let observed = root.join("install-build.txt");
        let observed_cwd = root.join("install-cwd.txt");
        let installer = root.join("cargo-installer");
        install_script(
            &installer,
            r#"#!/bin/sh
set -eu
test "$1" = install
    while [ "$#" -gt 0 ]; do
        if [ "$1" = --root ]; then
            shift
            install_root=$1
        fi
        shift
    done
    printf '%s\n' "$(cd "$CARGO_TARGET_DIR" && pwd -P)" > "$OBSERVED_BUILD"
    printf '%s\n' "$(pwd -P)" > "$OBSERVED_CWD"
    test -d "$CARGO_TARGET_DIR"
    test "$(pwd -P)" = "$(cd "$CARGO_TARGET_DIR" && pwd -P)"
    test "${RUSTFLAGS+set}" != set
    test "${CARGO_ENCODED_RUSTFLAGS+set}" != set
    test "${RUSTDOCFLAGS+set}" != set
    test "${CARGO_BUILD_TARGET+set}" != set
    touch "$CARGO_TARGET_DIR/temporary-build"
    mkdir -p "$install_root/bin"
    cp "$INSTALL_SOURCE" "$install_root/bin/cargo-nextest"
    exit 0
"#,
        );
        let cargo_home = root.join("cargo-home");
        let process = Process::new(
            root,
            BTreeMap::from([
                (
                    OsString::from("CARGO_HOME"),
                    cargo_home.clone().into_os_string(),
                ),
                (OsString::from("TMPDIR"), scratch.clone().into_os_string()),
                (
                    OsString::from("CARGO_TARGET_DIR"),
                    engine_target.clone().into_os_string(),
                ),
                (OsString::from("KITHARA_CACHE_TRUST"), OsString::from(trust)),
                (
                    OsString::from("PATH"),
                    inherited.environment_var("PATH").expect("inherited PATH"),
                ),
                (
                    OsString::from("OBSERVED_BUILD"),
                    observed.clone().into_os_string(),
                ),
                (
                    OsString::from("OBSERVED_CWD"),
                    observed_cwd.clone().into_os_string(),
                ),
                (OsString::from("INSTALL_SOURCE"), source.into_os_string()),
                (
                    OsString::from("RUSTFLAGS"),
                    OsString::from("--cfg engine_only"),
                ),
                (
                    OsString::from("CARGO_ENCODED_RUSTFLAGS"),
                    OsString::from("--cfg\u{1f}engine_only"),
                ),
                (
                    OsString::from("RUSTDOCFLAGS"),
                    OsString::from("--cfg engine_docs"),
                ),
                (
                    OsString::from("CARGO_BUILD_TARGET"),
                    OsString::from("wasm32-unknown-unknown"),
                ),
            ]),
        );
        let selected =
            nextest::prepare(&process, &pins, installer.to_str().expect("installer path"))
                .expect("install and validate real pinned nextest")
                .expect("a running lane selects its tool");
        let version = pins
            .cargo_tool_version("cargo-nextest")
            .expect("nextest pin");
        let cache = cargo_home.join("kithara-nextest").join(trust).join(format!(
            "{}-{}",
            env::consts::OS,
            env::consts::ARCH
        ));
        assert_eq!(selected, cache.join(version).join("bin"));
        let build = PathBuf::from(fs::read_to_string(observed).expect("observed build").trim());
        let cwd = PathBuf::from(
            fs::read_to_string(observed_cwd)
                .expect("observed install cwd")
                .trim(),
        );
        let physical_scratch = scratch.canonicalize().expect("physical scratch");
        assert_eq!(build.parent(), Some(physical_scratch.as_path()));
        assert_eq!(
            cwd, build,
            "installation runs outside the repository configuration"
        );
        assert!(
            !build.exists(),
            "installation build is removed before the lane runs"
        );
        assert_eq!(
            fs::read_dir(engine_target).expect("engine target").count(),
            0
        );
        assert!(!cache.join(version).join("target").exists());
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(cache.join(format!("{version}.lock")))
            .expect("installer lock");
        FileLock::try_exclusive(lock).expect("per-pin lock released before lane steps");
    }

    /// Each scope records the sources its own branches read, so the default
    /// branch publishes whichever scope it runs in, and a branch never does.
    #[test]
    fn only_the_default_branch_publishes_the_source_layer() {
        assert!(is_source_publisher(PipelineKind::Main.name()));
        assert!(!is_source_publisher(PipelineKind::Branch.name()));
        assert!(!is_source_publisher(PipelineKind::MergeRequest.name()));
        assert!(!is_source_publisher(PipelineKind::Quarantine.name()));
    }
}

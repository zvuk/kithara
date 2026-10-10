use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Result, bail};
use clap::Args;
use kithara_devtools::Ctx;
use tracing::{info, warn};

use super::declared;
use crate::{
    ci::{
        build_dir::{BuildDir, LaneTarget, Target},
        config::CiPins,
        environment::process_var,
        process::Process,
        run::PipelineKind,
    },
    config::{CiLaneConfig, KitharaExt},
    consts,
};

/// Run one declared lane in the environment the executor already prepared.
///
/// `ci run` is the other way into the same lane body: it prepares the cache
/// roots, the compiler cache and the build-cache lease first, because the
/// GitLab executor arrives with none of them. A GitHub job's container is
/// started with exactly those variables already set, so preparing them again
/// would be a second owner of the same state.
#[derive(Debug, Args)]
pub(crate) struct LaneArgs {
    /// The declared lane to run.
    lane: String,
    // Required, not defaulted: the only caller is a generated workflow that
    // already passes this on every invocation, so a default would only paper
    // over a resolution the caller owns.
    #[arg(long, value_enum)]
    kind: PipelineKind,
    /// A nextest filterset the lane's test suite is narrowed to.
    #[arg(long)]
    narrow: Option<String>,
}

fn lookup<'a>(lanes: &'a BTreeMap<String, CiLaneConfig>, name: &str) -> Result<&'a CiLaneConfig> {
    match lanes.get(name) {
        Some(lane) => Ok(lane),
        None => bail!(
            "`{name}` is not a declared CI lane; this repository has {}",
            lanes.keys().cloned().collect::<Vec<_>>().join(", ")
        ),
    }
}

/// What a lane is handed rather than works out: where this executor builds,
/// and the kind of pipeline it runs in.
///
/// Cargo writes where `CARGO_TARGET_DIR` names, and [`Process::target_dir`]
/// reads the same value, so the lane looks for its binaries where they were
/// written; the checkout is the wrong answer wherever the executor named
/// another directory. The kind arrives only as this process's argument, so a
/// step that reads it - the weekly health report adds semver-checks - would
/// otherwise never see it. Nothing else is copied: a child already inherits
/// this process's environment, and [`Process`] layers what it is given on top.
fn executor_vars(target_dir: Option<&Path>, kind: PipelineKind) -> BTreeMap<OsString, OsString> {
    let mut vars = BTreeMap::from([(
        OsString::from("KITHARA_PIPELINE_KIND"),
        OsString::from(kind.name()),
    )]);
    if let Some(target) = target_dir {
        vars.insert(
            OsString::from("CARGO_TARGET_DIR"),
            target.as_os_str().to_owned(),
        );
    }
    vars
}

pub(crate) fn run(args: &LaneArgs, ctx: &Ctx) -> Result<()> {
    run_in(args, ctx, &process_var)
}

fn run_in(args: &LaneArgs, ctx: &Ctx, var: &dyn Fn(&str) -> Option<OsString>) -> Result<()> {
    let ext = KitharaExt::from_ctx(ctx)?;
    ext.ci.validate()?;
    let lane = lookup(&ext.ci.lanes, &args.lane)?;
    let pins = CiPins::load(&ctx.root.join(&ext.ci.pins))?;
    let narrow = args.narrow.as_deref();
    if !declared::runs_anything(lane, args.kind, narrow, &ctx.root, &ctx.config)? {
        info!(
            lane = %args.lane,
            "the lane's suite selects no test lane on this branch; nothing to build"
        );
        return Ok(());
    }
    let target = Target::enter(
        &ctx.root,
        LaneTarget {
            name: &args.lane,
            window: ext.ci.lane_unit_window(),
        },
        var,
    )?;
    let process = Process::new(&ctx.root, executor_vars(target.cargo_dir(), args.kind));
    let result = crate::ci::run::journalled(&process, &args.lane, || {
        let result = declared::run(&process, lane, &pins, &ctx.config.tools, args.kind, narrow);
        if var("RUSTC_WRAPPER").is_some_and(|wrapper| !wrapper.is_empty()) {
            let on_github =
                crate::job::github_in(&|name| var(name).and_then(|value| value.into_string().ok()));
            crate::ci::run::note_compiler_cache(
                &process,
                ctx.config.tools.program("sccache"),
                on_github,
            );
        }
        result
    });
    if let Target::Alias { build, .. } = &target {
        publish_timings(build, var);
    }
    result
}

/// Publishes while the slot is still held. The job owns `RUNNER_TEMP`, which
/// GitHub empties at its start and end, so another job taking the released
/// slot cannot clear or replace the announced copy. Without both GitHub
/// variables there is no job to publish to. Failure never fails a lane.
fn publish_timings(build: &BuildDir, var: &dyn Fn(&str) -> Option<OsString>) {
    let (Some(env_file), Some(temp)) = (var("GITHUB_ENV"), var("RUNNER_TEMP")) else {
        return;
    };
    let destination = PathBuf::from(temp).join(consts::CARGO_TIMINGS_DIR);
    match build.copy_timings(&destination) {
        Ok(true) => announce(Path::new(&env_file), &destination),
        Ok(false) => {}
        Err(error) => warn!(
            "cannot copy this job's timing reports from {} to {}: {error:#}; their upload is skipped",
            build.path().display(),
            destination.display()
        ),
    }
}

/// Tells later steps where this job's copied timing reports are, only after
/// its build wrote one. GitHub reads `GITHUB_ENV` into later steps. A job
/// cancelled before this point tells nothing, so its upload is skipped.
/// Failure to tell never fails a lane.
fn announce(env_file: &Path, dir: &Path) {
    let written = OpenOptions::new()
        .append(true)
        .open(env_file)
        .and_then(|mut file| writeln!(file, "{}={}", consts::LANE_TIMINGS_ENV, dir.display()));
    if let Err(error) = written {
        warn!(
            "later steps cannot upload this job's timing reports in {}: writing {} failed: {error}",
            dir.display(),
            env_file.display()
        );
    }
}

#[cfg(test)]
mod tests {
    use std::{env, ffi::OsStr, fs, path::Path};

    use kithara_devtools::{common::project::TestCommandConfig, lease};

    use super::*;
    use crate::ci::{
        build_dir::fixture::git,
        config::{fixture, workspace_root},
    };

    /// A lane builds where the executor said. These runners are ephemeral and
    /// the checkout is deleted before the lane starts, so a build directory
    /// named from the checkout is empty on every job; the executor names one
    /// that outlives it, and the lane has to look for its binaries in that
    /// one.
    #[test]
    fn a_lane_builds_where_the_executor_said() {
        let root = Path::new("/runner/_work/kithara/kithara");

        let handed = Process::new(
            root,
            executor_vars(Some(Path::new("/cache/target")), PipelineKind::Branch),
        );
        let bare = Process::new(root, executor_vars(None, PipelineKind::Branch));

        assert_eq!(handed.target_dir(), Path::new("/cache/target"));
        assert_eq!(bare.target_dir(), root.join("target"));
    }

    /// The weekly health report runs semver-checks only when it reads the
    /// weekly kind, and a GitHub job hands the kind to this process alone.
    #[test]
    fn a_lane_tells_its_steps_the_kind_it_runs_in() {
        let process = Process::new(
            Path::new("/runner/_work/kithara/kithara"),
            executor_vars(None, PipelineKind::Weekly),
        );

        let command = process.command("just");

        assert!(
            command
                .get_envs()
                .any(|(key, value)| key == "KITHARA_PIPELINE_KIND"
                    && value == Some(OsStr::new("weekly"))),
            "{command:?}"
        );
    }

    // A lane name that is not in the catalog must answer with the catalog,
    // not with whatever the machine happens to be missing.
    #[test]
    fn an_unknown_lane_answers_with_the_lanes_this_repository_has() {
        let lanes = BTreeMap::from([("linux-lint".to_owned(), CiLaneConfig::default())]);
        let error = lookup(&lanes, "linux-lnt").expect_err("a misspelled lane is refused");
        assert!(
            error.to_string().contains("linux-lint"),
            "the error must list the lanes: {error}"
        );
    }

    #[test]
    fn a_known_lane_is_returned() {
        let lanes = BTreeMap::from([
            (
                "linux-lint".to_owned(),
                CiLaneConfig {
                    label: "linux-lint".to_owned(),
                    ..CiLaneConfig::default()
                },
            ),
            (
                "apple-test".to_owned(),
                CiLaneConfig {
                    label: "apple-test".to_owned(),
                    ..CiLaneConfig::default()
                },
            ),
        ]);
        let found = lookup(&lanes, "apple-test").expect("a declared lane is found");
        assert_eq!(
            found.label, "apple-test",
            "lookup must return the lane that was asked for, not merely any lane"
        );
    }

    /// The executor's variables as `ci lane` reads them.
    fn environment<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<OsString> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| OsString::from(*value))
        }
    }

    /// The job announces exactly one directory for its later upload step.
    #[cfg(unix)]
    fn announced(github_env: &Path) -> PathBuf {
        let contents = fs::read_to_string(github_env).expect("read GITHUB_ENV");
        let lines = contents.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 1, "the job announces exactly one directory");
        let dir = lines[0]
            .strip_prefix(&format!("{}=", consts::LANE_TIMINGS_ENV))
            .expect("the job announces its timing directory");
        assert!(!dir.is_empty(), "the announced directory is not empty");
        PathBuf::from(dir)
    }

    /// Every uploaded file and its content, following directory links like the upload.
    #[cfg(unix)]
    fn reports(dir: &Path) -> BTreeMap<String, String> {
        let mut found = BTreeMap::new();
        for entry in fs::read_dir(dir).expect("list the upload directory") {
            let entry = entry.expect("read an upload entry");
            let path = entry.path();
            let name = entry
                .file_name()
                .into_string()
                .expect("a UTF-8 upload path");
            let metadata = fs::metadata(&path).expect("read upload entry metadata");
            if metadata.is_dir() {
                for (relative, content) in reports(&path) {
                    found.insert(format!("{name}/{relative}"), content);
                }
            } else if metadata.is_file() {
                found.insert(
                    name,
                    fs::read_to_string(&path).expect("read an uploaded file"),
                );
            }
        }
        found
    }

    /// A workspace at `root` declaring one lane whose only step succeeds.
    fn trivial_lane(root: &Path) -> (Ctx, LaneArgs) {
        if cfg!(windows) {
            lane_running(root, "cmd", r#"args = ["/C", "exit", "0"]"#)
        } else {
            lane_running(root, "sh", r#"args = ["-c", "exit 0"]"#)
        }
    }

    /// A checkout at `root` declaring one lane whose only step, labelled
    /// `run`, runs `program` as the TOML lines `step` say.
    fn lane_running(root: &Path, program: &str, step: &str) -> (Ctx, LaneArgs) {
        let root = root.to_path_buf();
        git(&root, &["init", "-q"]);
        fixture()
            .pins
            .write(&root.join("ci-pins.toml"))
            .expect("write fixture pins into the temporary workspace");

        // The fixture runs here, so it names here: a lane refuses a machine
        // that is not the one it declared, and that refusal is the subject of
        // other tests, not of this one.
        let os = env::consts::OS;
        let config_text = format!(
            r#"
[ext.ci]
pins = "ci-pins.toml"

[ext.ci.lanes.trivial]
cache_group = "host"
label = "fixture"
os = "{os}"
program = "{program}"
role = "gate"
timeout_minutes = 1

[[ext.ci.lanes.trivial.steps]]
label = "run"
{step}
"#
        );
        let ctx = Ctx::new(
            root,
            toml::from_str(&config_text).expect("parse fixture lane config"),
        );
        let args = LaneArgs {
            lane: "trivial".to_owned(),
            kind: PipelineKind::Branch,
            narrow: None,
        };
        (ctx, args)
    }

    /// `ci run` requires `KITHARA_CI_HOST_CONFIG` and bails without it
    /// (`xtask/src/ci/run.rs`). So a lane that reaches its own work at all,
    /// with the ambient environment left untouched, is already the proof
    /// that `ci lane` resolved no host profile: the fixture lane's one step
    /// is `sh -c "exit 0"` (`cmd /C exit 0` on Windows), and `Ok` means
    /// execution got there.
    #[test]
    fn a_lane_reaches_its_own_work_with_no_host_profile_resolved() {
        let temp = tempfile::tempdir().expect("create fixture workspace");
        let (ctx, args) = trivial_lane(temp.path());

        let result = run_in(&args, &ctx, &environment(&[]));

        assert!(
            result.is_ok(),
            "a lane that needs no host profile must not fail resolving one: {result:?}"
        );
    }

    /// In a CI job a lane builds in a directory of its own beside the alias
    /// the executor named, while Cargo is told the alias: every lane compiles
    /// at the one path, so the compiler cache's keys match across lanes and
    /// runners.
    #[cfg(unix)]
    #[test]
    fn a_lane_in_a_ci_job_builds_in_its_own_directory_behind_the_alias() {
        let temp = tempfile::tempdir().expect("create fixture workspace");
        let builds = tempfile::tempdir().expect("create the runner's build root");
        let seen = temp.path().join("seen");
        let record = format!(
            r#"args = ["-c", "printf '%s' \"$CARGO_TARGET_DIR\" > '{}'"]"#,
            seen.display()
        );
        let (ctx, args) = lane_running(temp.path(), "sh", &record);
        let github_env = temp.path().join("github-env");
        fs::write(&github_env, "").expect("create the job's GITHUB_ENV");
        let alias = builds.path().join(consts::BUILD_ALIAS);

        run_in(
            &args,
            &ctx,
            &environment(&[
                ("CI", "true"),
                (
                    "CARGO_TARGET_DIR",
                    builds.path().to_str().expect("a UTF-8 build root"),
                ),
                (
                    "GITHUB_ENV",
                    github_env.to_str().expect("a UTF-8 GITHUB_ENV"),
                ),
            ]),
        )
        .expect("lane runs");

        assert_eq!(
            fs::read_to_string(&seen).expect("the step recorded its build path"),
            alias.display().to_string(),
            "Cargo is told the alias every lane shares"
        );
        let own = fs::read_link(&alias).unwrap();
        assert_eq!(own.parent(), Some(builds.path()));
        assert!(
            own.join(lease::FILE).is_file(),
            "the lane leased its own build"
        );
        assert_eq!(
            fs::canonicalize(temp.path().join("target")).unwrap(),
            fs::canonicalize(&own).unwrap(),
            "artifact paths under the checkout's target reach the lane's build"
        );
    }

    /// A build that wrote no report announces nothing, so it cannot upload an
    /// earlier job's report from a warm slot.
    #[cfg(unix)]
    #[test]
    fn a_lane_whose_build_wrote_no_timing_report_tells_the_job_nothing() {
        let temp = tempfile::tempdir().expect("create fixture workspace");
        let builds = tempfile::tempdir().expect("create the runner's build root");
        let (ctx, args) = trivial_lane(temp.path());
        let earlier = builds
            .path()
            .join("trivial-0/cargo-timings/cargo-timing.html");
        fs::create_dir_all(earlier.parent().unwrap()).expect("create the earlier timing directory");
        fs::write(&earlier, "earlier report").expect("write the earlier job's timing report");
        let github_env = temp.path().join("github-env");
        fs::write(&github_env, "").expect("create the job's GITHUB_ENV");

        run_in(
            &args,
            &ctx,
            &environment(&[
                ("CI", "true"),
                (
                    "CARGO_TARGET_DIR",
                    builds.path().to_str().expect("a UTF-8 build root"),
                ),
                (
                    "GITHUB_ENV",
                    github_env.to_str().expect("a UTF-8 GITHUB_ENV"),
                ),
            ]),
        )
        .expect("lane runs");

        assert_eq!(
            fs::read_to_string(&github_env).expect("read GITHUB_ENV"),
            "",
            "a job whose build wrote no report must not upload another job's"
        );
        assert!(
            !earlier.exists(),
            "the earlier job's timing report must not survive slot entry"
        );
    }

    /// A finished lane announces a job-owned copy containing only its report,
    /// so releasing the build slot cannot change the upload.
    #[cfg(unix)]
    #[test]
    fn a_lane_tells_the_job_the_timing_report_its_build_wrote() {
        let temp = tempfile::tempdir().expect("create fixture workspace");
        let builds = tempfile::tempdir().expect("create the runner's build root");
        let (ctx, args) = lane_running(
            temp.path(),
            "sh",
            r#"args = ["-c", "mkdir -p \"$CARGO_TARGET_DIR/cargo-timings\" && echo report > \"$CARGO_TARGET_DIR/cargo-timings/cargo-timing.html\""]"#,
        );
        let github_env = temp.path().join("github-env");
        fs::write(&github_env, "").expect("create the job's GITHUB_ENV");
        let runner_temp = tempfile::tempdir_in(temp.path()).expect("create the job's RUNNER_TEMP");

        run_in(
            &args,
            &ctx,
            &environment(&[
                ("CI", "true"),
                (
                    "CARGO_TARGET_DIR",
                    builds.path().to_str().expect("a UTF-8 build root"),
                ),
                (
                    "GITHUB_ENV",
                    github_env.to_str().expect("a UTF-8 GITHUB_ENV"),
                ),
                (
                    "RUNNER_TEMP",
                    runner_temp.path().to_str().expect("a UTF-8 RUNNER_TEMP"),
                ),
            ]),
        )
        .expect("lane runs");

        let upload = announced(&github_env);
        assert_eq!(
            reports(&upload),
            BTreeMap::from([(
                "cargo-timings/cargo-timing.html".to_owned(),
                "report\n".to_owned(),
            )])
        );
        assert!(
            !upload.starts_with(builds.path()),
            "the job owns the upload outside the reusable build root"
        );
    }

    /// A failed lane still announces a job-owned copy of its report, so its
    /// timing evidence survives release of the build slot.
    #[cfg(unix)]
    #[test]
    fn a_failed_lane_still_tells_the_job_the_timing_report_its_build_wrote() {
        let temp = tempfile::tempdir().expect("create fixture workspace");
        let builds = tempfile::tempdir().expect("create the runner's build root");
        let (ctx, args) = lane_running(
            temp.path(),
            "sh",
            r#"args = ["-c", "mkdir -p \"$CARGO_TARGET_DIR/cargo-timings\" && echo report > \"$CARGO_TARGET_DIR/cargo-timings/cargo-timing.html\"; exit 1"]"#,
        );
        let github_env = temp.path().join("github-env");
        fs::write(&github_env, "").expect("create the job's GITHUB_ENV");
        let runner_temp = tempfile::tempdir_in(temp.path()).expect("create the job's RUNNER_TEMP");

        let result = run_in(
            &args,
            &ctx,
            &environment(&[
                ("CI", "true"),
                (
                    "CARGO_TARGET_DIR",
                    builds.path().to_str().expect("a UTF-8 build root"),
                ),
                (
                    "GITHUB_ENV",
                    github_env.to_str().expect("a UTF-8 GITHUB_ENV"),
                ),
                (
                    "RUNNER_TEMP",
                    runner_temp.path().to_str().expect("a UTF-8 RUNNER_TEMP"),
                ),
            ]),
        );

        assert!(result.is_err(), "the failed step must fail its lane");
        let upload = announced(&github_env);
        assert_eq!(
            reports(&upload),
            BTreeMap::from([(
                "cargo-timings/cargo-timing.html".to_owned(),
                "report\n".to_owned(),
            )])
        );
        assert!(
            !upload.starts_with(builds.path()),
            "the job owns the upload outside the reusable build root"
        );
    }

    /// A job's uploaded report survives the next job clearing the same build slot.
    #[cfg(unix)]
    #[test]
    fn a_report_a_job_announced_survives_the_next_job_taking_its_slot() {
        let temp = tempfile::tempdir().expect("create fixture workspace");
        let builds = temp.path().join("builds");
        let checkout_a = tempfile::tempdir_in(temp.path()).expect("create job A's checkout");
        let runner_temp_a = tempfile::tempdir_in(temp.path()).expect("create job A's RUNNER_TEMP");
        let github_env_a = temp.path().join("github-env-a");
        fs::write(&github_env_a, "").expect("create job A's GITHUB_ENV");
        let (ctx_a, args_a) = lane_running(
            checkout_a.path(),
            "sh",
            r#"args = ["-c", "mkdir -p \"$CARGO_TARGET_DIR/cargo-timings\" && echo report > \"$CARGO_TARGET_DIR/cargo-timings/cargo-timing.html\""]"#,
        );

        run_in(
            &args_a,
            &ctx_a,
            &environment(&[
                ("CI", "true"),
                (
                    "CARGO_TARGET_DIR",
                    builds.to_str().expect("a UTF-8 build root"),
                ),
                (
                    "GITHUB_ENV",
                    github_env_a.to_str().expect("a UTF-8 GITHUB_ENV"),
                ),
                (
                    "RUNNER_TEMP",
                    runner_temp_a.path().to_str().expect("a UTF-8 RUNNER_TEMP"),
                ),
            ]),
        )
        .expect("job A's lane runs");
        let upload = announced(&github_env_a);
        let slot = builds.join("trivial-0");
        assert_eq!(
            fs::read_link(builds.join(consts::BUILD_ALIAS)).unwrap(),
            slot
        );

        let checkout_b = tempfile::tempdir_in(temp.path()).expect("create job B's checkout");
        let runner_temp_b = tempfile::tempdir_in(temp.path()).expect("create job B's RUNNER_TEMP");
        let github_env_b = temp.path().join("github-env-b");
        fs::write(&github_env_b, "").expect("create job B's GITHUB_ENV");
        let (ctx_b, args_b) = trivial_lane(checkout_b.path());

        run_in(
            &args_b,
            &ctx_b,
            &environment(&[
                ("CI", "true"),
                (
                    "CARGO_TARGET_DIR",
                    builds.to_str().expect("a UTF-8 build root"),
                ),
                (
                    "GITHUB_ENV",
                    github_env_b.to_str().expect("a UTF-8 GITHUB_ENV"),
                ),
                (
                    "RUNNER_TEMP",
                    runner_temp_b.path().to_str().expect("a UTF-8 RUNNER_TEMP"),
                ),
            ]),
        )
        .expect("job B's lane runs");

        assert_eq!(
            fs::read_link(builds.join(consts::BUILD_ALIAS)).unwrap(),
            slot
        );
        assert_eq!(
            reports(&upload),
            BTreeMap::from([(
                "cargo-timings/cargo-timing.html".to_owned(),
                "report\n".to_owned(),
            )]),
            "job A's report survives job B taking and clearing the same slot"
        );
        assert_eq!(
            fs::read_to_string(&github_env_b).expect("read job B's GITHUB_ENV"),
            "",
            "job B wrote no report and announces nothing"
        );
    }

    /// The upload contains only owned reports; directory links neither upload
    /// another build's report nor let clearing delete it.
    #[cfg(unix)]
    #[test]
    fn a_lane_tells_the_job_only_the_reports_its_build_directory_holds() {
        let temp = tempfile::tempdir().expect("create fixture workspace");
        let builds = temp.path().join("builds");
        let slot = builds.join("trivial-0");
        let elsewhere = temp.path().join("elsewhere");
        let linked_report = elsewhere.join("cargo-timings/cargo-timing.html");
        fs::create_dir_all(linked_report.parent().unwrap())
            .expect("create the linked timing directory");
        fs::write(&linked_report, "elsewhere").expect("write another build's report");
        fs::create_dir_all(&slot).expect("create the build slot");
        std::os::unix::fs::symlink(&elsewhere, slot.join("linked")).expect("link another build");
        let (ctx, args) = lane_running(
            temp.path(),
            "sh",
            r#"args = ["-c", "mkdir -p \"$CARGO_TARGET_DIR/cargo-timings\" \"$CARGO_TARGET_DIR/nested/cargo-timings\" && echo report > \"$CARGO_TARGET_DIR/cargo-timings/cargo-timing.html\" && echo report > \"$CARGO_TARGET_DIR/nested/cargo-timings/cargo-timing.html\""]"#,
        );
        let github_env = temp.path().join("github-env");
        fs::write(&github_env, "").expect("create the job's GITHUB_ENV");
        let runner_temp = tempfile::tempdir_in(temp.path()).expect("create the job's RUNNER_TEMP");

        run_in(
            &args,
            &ctx,
            &environment(&[
                ("CI", "true"),
                (
                    "CARGO_TARGET_DIR",
                    builds.to_str().expect("a UTF-8 build root"),
                ),
                (
                    "GITHUB_ENV",
                    github_env.to_str().expect("a UTF-8 GITHUB_ENV"),
                ),
                (
                    "RUNNER_TEMP",
                    runner_temp.path().to_str().expect("a UTF-8 RUNNER_TEMP"),
                ),
            ]),
        )
        .expect("lane runs");

        assert_eq!(
            reports(&announced(&github_env)),
            BTreeMap::from([
                (
                    "cargo-timings/cargo-timing.html".to_owned(),
                    "report\n".to_owned(),
                ),
                (
                    "nested/cargo-timings/cargo-timing.html".to_owned(),
                    "report\n".to_owned(),
                ),
            ]),
            "the upload excludes reports reached through directory links"
        );
        assert!(
            linked_report.is_file(),
            "clearing must not delete another build's report through a directory link"
        );
    }

    /// The rebuild check asks cargo what the next job of this commit would
    /// build, and fails the lane on any unit named; a suite the next job
    /// reuses whole passes. It needs nothing but the build the step left.
    #[cfg(unix)]
    #[test]
    fn a_rebuild_check_fails_a_suite_the_next_job_would_build_again() {
        use std::os::unix::fs::PermissionsExt as _;

        let temp = tempfile::tempdir().expect("create fixture workspace");
        let status = temp.path().join("status");
        let just = temp.path().join("just");
        fs::write(
            &just,
            format!(
                "#!/bin/sh\ncase \"$*\" in *--no-run*) cat '{}' >&2 ;; esac\n",
                status.display()
            ),
        )
        .expect("write the just double");
        fs::set_permissions(&just, fs::Permissions::from_mode(0o755))
            .expect("make the just double runnable");
        let (mut ctx, args) = lane_running(
            temp.path(),
            "just",
            "args = [\"test\", \"run\", \"--timings\"]\nrebuild_check = true",
        );
        ctx.config.tools = toml::from_str(&format!("[just]\nprogram = \"{}\"\n", just.display()))
            .expect("parse the tools table");
        ctx.config.test = test_lanes();
        let anywhere = environment(&[]);

        fs::write(&status, "   Compiling probe v0.0.0 (/w)\n").expect("write cargo's answer");
        let error = run_in(&args, &ctx, &anywhere)
            .expect_err("a unit the next job would build fails the lane");
        assert!(
            format!("{error:#}").contains("Compiling probe v0.0.0"),
            "{error:#}"
        );

        fs::write(&status, "       Fresh probe v0.0.0 (/w)\n").expect("write cargo's answer");
        run_in(&args, &ctx, &anywhere).expect("a suite the next job reuses whole passes");
    }

    /// A checkout on branch `topic`, one commit past `origin/main`, that
    /// changed `changed`, declaring one lane whose step runs the touched suite
    /// through a `just` that records whether it ran. The `workspace` lane is
    /// the default and `tooling` owns `xtask/`.
    #[cfg(unix)]
    fn touched_lane(root: &Path, changed: &str) -> (Ctx, LaneArgs, PathBuf) {
        use std::os::unix::fs::PermissionsExt as _;

        let (mut ctx, args) = lane_running(
            root,
            "just",
            r#"args = ["test", "run", "--touched", "--timings"]"#,
        );
        let commit = |message: &str| {
            git(
                root,
                &[
                    "-c",
                    "user.name=fixture",
                    "-c",
                    "user.email=fixture@example.invalid",
                    "commit",
                    "--allow-empty",
                    "--no-gpg-sign",
                    "-qm",
                    message,
                ],
            );
        };
        git(root, &["checkout", "-qb", "main"]);
        commit("base");
        git(
            root,
            &[
                "remote",
                "add",
                "origin",
                root.to_str().expect("a UTF-8 root"),
            ],
        );
        git(root, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        git(root, &["checkout", "-qb", "topic"]);
        let path = root.join(changed);
        fs::create_dir_all(path.parent().expect("a changed file has a directory")).unwrap();
        fs::write(&path, "changed").unwrap();
        git(root, &["add", changed]);
        commit("change");

        let called = root.join("called");
        let just = root.join("just");
        fs::write(
            &just,
            format!("#!/bin/sh\nprintf ran > '{}'\n", called.display()),
        )
        .unwrap();
        fs::set_permissions(&just, fs::Permissions::from_mode(0o755)).unwrap();
        ctx.config.tools = toml::from_str(&format!("[just]\nprogram = \"{}\"\n", just.display()))
            .expect("parse the tools table");
        ctx.config.test = test_lanes();
        (ctx, args, called)
    }

    /// Test lanes for a suite step to select from: the default `workspace`
    /// lane, and `tooling`, which owns `xtask/` and builds what the default
    /// lane leaves out.
    fn test_lanes() -> TestCommandConfig {
        toml::from_str(
            r#"
default_lane = "workspace"
default_backend = "http"
nextest_config = ".config/nextest.toml"
[net_backends.http]
[lanes.workspace.cargo]
workspace = true
exclude = ["tools"]
[lanes.tooling]
owns = ["xtask/"]
cargo.packages = ["tools"]
"#,
        )
        .expect("parse the test lanes")
    }

    /// Runs a touched lane as a CI job would, building under `builds`, and
    /// returns what the job's later steps were told.
    #[cfg(unix)]
    fn run_touched(root: &Path, changed: &str, builds: &Path) -> (Result<()>, String, PathBuf) {
        let (ctx, args, called) = touched_lane(root, changed);
        let github_env = root.join("github-env");
        fs::write(&github_env, "").expect("create the job's GITHUB_ENV");
        let result = run_in(
            &args,
            &ctx,
            &environment(&[
                ("CI", "true"),
                (
                    "CARGO_TARGET_DIR",
                    builds.to_str().expect("a UTF-8 build root"),
                ),
                (
                    "GITHUB_ENV",
                    github_env.to_str().expect("a UTF-8 GITHUB_ENV"),
                ),
            ]),
        );
        (result, fs::read_to_string(github_env).unwrap(), called)
    }

    /// A lane whose suite runs only what the branch touched, on a branch that
    /// touched none of its lanes, has nothing to build: it claims no build
    /// directory, tells the job no build to upload timings from, and runs
    /// nothing.
    #[cfg(unix)]
    #[test]
    fn a_touched_lane_that_selects_nothing_claims_no_build() {
        let temp = tempfile::tempdir().expect("create fixture workspace");
        let builds = tempfile::tempdir().expect("create the runner's build root");

        let (result, told, called) = run_touched(temp.path(), "xtask/probe.rs", builds.path());

        result.expect("a lane with nothing selected succeeds");
        assert!(
            fs::read_dir(builds.path()).unwrap().next().is_none(),
            "a lane with nothing selected claimed a build directory"
        );
        assert_eq!(told, "", "a lane with nothing selected announced a build");
        assert!(
            !called.exists(),
            "a lane with nothing selected ran its suite"
        );
    }

    /// A path no lane owns runs the default lane, so the same lane builds.
    #[cfg(unix)]
    #[test]
    fn a_touched_lane_that_selects_a_lane_builds_it() {
        let temp = tempfile::tempdir().expect("create fixture workspace");
        let builds = tempfile::tempdir().expect("create the runner's build root");

        let (result, _told, called) = run_touched(temp.path(), "crates/probe.rs", builds.path());

        result.expect("the selected lane runs");
        assert!(called.exists(), "the selected suite must run");
        let own = fs::read_link(builds.path().join(consts::BUILD_ALIAS)).unwrap();
        assert!(
            own.join(lease::FILE).is_file(),
            "the selected lane leased its own build"
        );
    }

    /// A job compiling through the cache says what the cache carried for it;
    /// one that compiles without it has no cache to ask.
    #[cfg(unix)]
    #[test]
    fn a_lane_that_compiled_through_the_cache_reads_its_counts() {
        use std::os::unix::fs::PermissionsExt as _;

        let temp = tempfile::tempdir().expect("create fixture workspace");
        let asked = temp.path().join("asked");
        let sccache = temp.path().join("sccache");
        fs::write(
            &sccache,
            format!(
                "#!/bin/sh\necho \"$@\" >> '{}'\nprintf 'Cache hits 3\\nCache misses 1\\nCache write errors 0\\n'\n",
                asked.display()
            ),
        )
        .expect("write the cache double");
        fs::set_permissions(&sccache, fs::Permissions::from_mode(0o755))
            .expect("make the cache double runnable");
        let (mut ctx, args) = trivial_lane(temp.path());
        ctx.config.tools =
            toml::from_str(&format!("[sccache]\nprogram = \"{}\"\n", sccache.display()))
                .expect("parse the tools table");

        run_in(&args, &ctx, &environment(&[])).expect("lane runs without a wrapper");
        assert!(
            !asked.exists(),
            "a lane with no wrapper has no cache to ask"
        );

        run_in(&args, &ctx, &environment(&[("RUSTC_WRAPPER", "sccache")]))
            .expect("lane runs through the wrapper");
        assert_eq!(
            fs::read_to_string(&asked).expect("the cache was asked"),
            "--show-stats\n"
        );
    }

    /// Outside a CI job a directory named like an alias is still just where
    /// Cargo was told to build: nothing is linked, and the checkout's own
    /// `target`, a developer's build, is left alone.
    #[cfg(unix)]
    #[test]
    fn a_lane_outside_a_ci_job_enters_no_build_directory() {
        let temp = tempfile::tempdir().expect("create fixture workspace");
        let builds = tempfile::tempdir().expect("create a build root");
        let (ctx, args) = trivial_lane(temp.path());
        let own = temp.path().join("target/debug/own-build");
        fs::create_dir_all(own.parent().expect("a build file has a directory"))
            .expect("create the developer's build");
        fs::write(&own, "built by hand").expect("write the developer's build");
        let named = builds.path().join(consts::BUILD_ALIAS);

        run_in(
            &args,
            &ctx,
            &environment(&[("CARGO_TARGET_DIR", named.to_str().expect("a UTF-8 path"))]),
        )
        .expect("lane runs");

        assert!(own.exists(), "the developer's build must survive the lane");
        assert!(
            fs::read_dir(builds.path()).unwrap().next().is_none(),
            "a lane outside a CI job entered a build directory"
        );
    }

    /// A job whose `GITHUB_ENV` cannot be written still runs its lane;
    /// announcing the job-owned report copy never determines lane success.
    #[cfg(unix)]
    #[test]
    fn a_lane_that_cannot_tell_the_job_where_it_built_still_runs() {
        let temp = tempfile::tempdir().expect("create fixture workspace");
        let builds = tempfile::tempdir().expect("create the runner's build root");
        let (ctx, args) = lane_running(
            temp.path(),
            "sh",
            r#"args = ["-c", "mkdir -p \"$CARGO_TARGET_DIR/cargo-timings\" && echo report > \"$CARGO_TARGET_DIR/cargo-timings/cargo-timing.html\""]"#,
        );
        let github_env = temp.path().join("missing/github-env");
        let runner_temp = tempfile::tempdir_in(temp.path()).expect("create the job's RUNNER_TEMP");

        let result = run_in(
            &args,
            &ctx,
            &environment(&[
                ("CI", "true"),
                (
                    "CARGO_TARGET_DIR",
                    builds.path().to_str().expect("a UTF-8 build root"),
                ),
                (
                    "GITHUB_ENV",
                    github_env.to_str().expect("a UTF-8 GITHUB_ENV"),
                ),
                (
                    "RUNNER_TEMP",
                    runner_temp.path().to_str().expect("a UTF-8 RUNNER_TEMP"),
                ),
            ]),
        );

        assert!(
            result.is_ok(),
            "a lane failed for the path only the timings upload reads: {result:?}"
        );
        let own = fs::read_link(builds.path().join(consts::BUILD_ALIAS)).unwrap();
        assert!(
            own.join(lease::FILE).is_file(),
            "the lane must still build in its own directory"
        );
    }

    /// The test above cannot fail loudly enough alone: a resolution bug that
    /// only sometimes needs a host profile could still return `Ok`. This
    /// pins the negative direction against the source directly: a GitHub
    /// container has no host profile installed, so this entrypoint must
    /// never name the machinery that would resolve one.
    ///
    /// Only the production half of this file is scanned - up to the
    /// `#[cfg(test)]` boundary - because the forbidden names below are
    /// themselves text inside this test module, and a census that read its
    /// own assertion would always trip on its own data.
    #[test]
    fn ci_lane_never_names_host_profile_machinery() {
        let source = fs::read_to_string(workspace_root().join("xtask/src/ci/lane/direct.rs"))
            .expect("direct.rs is readable");
        let production = source
            .split("#[cfg(test)]")
            .next()
            .expect("direct.rs has a production half before its test module");
        // A census that scanned nothing would pass every assertion below. The
        // split is a text match, so an earlier `#[cfg(test)]` would truncate
        // the production half silently; this is what makes that loud.
        assert!(
            production.contains("fn run(args: &LaneArgs"),
            "the production half must still hold the entrypoint being censused"
        );
        for forbidden in ["CiConfig::load", "CiEnvironment", "KITHARA_CI_HOST_CONFIG"] {
            assert!(
                !production.contains(forbidden),
                "a GitHub container has no host profile, so `ci lane` must never resolve \
                 one; found `{forbidden}` in direct.rs's production code"
            );
        }
    }

    #[test]
    fn declared_lanes_own_target_snapshot_lifecycle_for_both_executors() {
        let root = workspace_root();
        let direct = fs::read_to_string(root.join("xtask/src/ci/lane/direct.rs"))
            .expect("direct lane source is readable");
        let declared = fs::read_to_string(root.join("xtask/src/ci/lane/declared.rs"))
            .expect("declared lane source is readable");
        let production = |source: String| {
            source
                .split("#[cfg(test)]")
                .next()
                .expect("lane source has a production half")
                .to_owned()
        };

        assert!(!production(direct).contains("snapshot::"));
        let declared = production(declared);
        assert!(declared.contains("snapshot::restore_for_lane"));
        assert!(declared.contains("snapshot::publish_for_lane"));
        assert!(declared.contains("could not publish optional target snapshot"));
    }
}

use std::{collections::BTreeMap, env, ffi::OsString};

use anyhow::{Result, bail};
use clap::Args;
use kithara_devtools::Ctx;

use super::declared;
use crate::{
    ci::{config::CiPins, process::Process, run::PipelineKind},
    config::{CiLaneConfig, KitharaExt},
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

/// The one thing a lane is handed rather than works out: where this executor
/// builds.
///
/// A step spells its own build directory as `{target}`, and the checkout is
/// the wrong answer wherever the executor named another one - Cargo would
/// write where it was told while the lane looked for the binaries somewhere
/// nothing had written. Nothing else is copied: a child already inherits this
/// process's environment, and [`Process`] layers what it is given on top.
fn executor_vars(target_dir: Option<OsString>) -> BTreeMap<OsString, OsString> {
    target_dir
        .map(|target| BTreeMap::from([(OsString::from("CARGO_TARGET_DIR"), target)]))
        .unwrap_or_default()
}

pub(crate) fn run(args: &LaneArgs, ctx: &Ctx) -> Result<()> {
    let ext = KitharaExt::from_ctx(ctx)?;
    ext.ci.validate()?;
    let lane = lookup(&ext.ci.lanes, &args.lane)?;
    let pins = CiPins::load(&ctx.root.join(&ext.ci.pins))?;
    let vars = executor_vars(env::var_os("CARGO_TARGET_DIR"));
    let process = Process::new(&ctx.root, vars);
    declared::run(&process, lane, &pins, &ctx.config.tools, args.kind)
}

#[cfg(test)]
mod tests {
    use std::{env, fs, path::Path};

    use super::*;
    use crate::ci::config::{fixture, workspace_root};

    /// A lane builds where the executor said. These runners are ephemeral and
    /// the checkout is deleted before the lane starts, so a build directory
    /// named from the checkout is empty on every job; the executor names one
    /// that outlives it, and a step's `{target}` has to mean that one or the
    /// lane looks for its binaries where nothing wrote any.
    #[test]
    fn a_lane_builds_where_the_executor_said() {
        let root = Path::new("/runner/_work/kithara/kithara");

        let handed = Process::new(root, executor_vars(Some(OsString::from("/cache/target"))));
        let bare = Process::new(root, executor_vars(None));

        assert_eq!(handed.target_dir(), Path::new("/cache/target"));
        assert_eq!(bare.target_dir(), root.join("target"));
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

    /// `ci run` requires `KITHARA_CI_HOST_CONFIG` and bails without it
    /// (`xtask/src/ci/run.rs`). So a lane that reaches its own work at all,
    /// with the ambient environment left untouched, is already the proof
    /// that `ci lane` resolved no host profile: the fixture lane's one step
    /// is `sh -c "exit 0"` (`cmd /C exit 0` on Windows), and `Ok` means
    /// execution got there.
    #[test]
    fn a_lane_reaches_its_own_work_with_no_host_profile_resolved() {
        let (program, step_args) = if cfg!(windows) {
            ("cmd", r#"["/C", "exit", "0"]"#)
        } else {
            ("sh", r#"["-c", "exit 0"]"#)
        };

        let temp = tempfile::tempdir().expect("create fixture workspace");
        let root = temp.path().to_path_buf();
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
args = {step_args}
"#
        );
        let ctx = Ctx::new(
            root,
            toml::from_str(&config_text).expect("parse fixture lane config"),
        );
        let args = LaneArgs {
            lane: "trivial".to_owned(),
            kind: PipelineKind::Branch,
        };

        let result = run(&args, &ctx);

        assert!(
            result.is_ok(),
            "a lane that needs no host profile must not fail resolving one: {result:?}"
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
}

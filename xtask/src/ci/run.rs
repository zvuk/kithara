use std::{env, ffi::OsStr, path::PathBuf, process::Output};

use anyhow::{Context, Result, bail};
use clap::{Args, ValueEnum};
use kithara_devtools::Ctx;
use tracing::{info, warn};

use super::{
    config::CiConfig,
    environment::{CiEnvironment, PROVISIONED_LINUX_IMAGE_ENV, is_gitlab},
    lane,
    process::Process,
    verdict,
};
use crate::config::KitharaExt;

/// One CI job. Lanes are deliberately narrow: a job that does one thing can be
/// retried, skipped, or read on its own, and the step that failed is the job
/// name rather than a line buried in an hour of output. Where two lanes need
/// the same build, they repeat it — the executor keeps a warm target directory,
/// so repeating is cheaper than threading artifacts between jobs.
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum Lane {
    AppleLint,
    AppleMsrv,
    AppleTest,
    AppleTestFlashOff,
    AppleE2e,
    AppleXcframework,
    AppleSwiftTest,
    AppleIos,
    AppleIosTest,
    AppleSafari,
    LinuxSecrets,
    LinuxCheck,
    LinuxWasm,
    LinuxTest,
    LinuxDoc,
    LinuxLoom,
    LinuxBroadcast,
    LinuxIntegrationRegressions,
    LinuxSeleniumFirefox,
    LinuxCoverage,
    AndroidBuild,
    AndroidTest,
    WebChromium,
    WebFirefox,
    WebSize,
    WindowsArm64,
    WindowsX64,
    WindowsX64Build,
    DeepRtsan,
    DeepPerf,
    DeepBench,
    DepsDeny,
    DepsUnused,
    DepsFeatures,
    DepsSemver,
    ReleaseXcframework,
    ReleaseDocs,
    ReleaseWasm,
    ReleaseAndroid,
    ReleasePublish,
    Verdict,
}

/// Which shared cache a lane leases. Lanes sharing an executor share its cache,
/// so this follows the runner a job is tagged for, not the operating system it
/// happens to observe.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CacheGroup {
    Macos,
    Linux,
    Windows,
    Host,
}

impl Lane {
    pub(crate) const fn cache_group(self) -> CacheGroup {
        match self {
            Self::AppleLint
            | Self::AppleMsrv
            | Self::AppleTest
            | Self::AppleTestFlashOff
            | Self::AppleE2e
            | Self::AppleXcframework
            | Self::AppleSwiftTest
            | Self::AppleIos
            | Self::AppleIosTest
            | Self::AppleSafari
            | Self::DeepRtsan
            | Self::DeepPerf
            | Self::DeepBench
            | Self::ReleaseXcframework
            | Self::ReleaseDocs
            | Self::ReleaseWasm
            | Self::Verdict => CacheGroup::Macos,
            Self::LinuxSecrets
            | Self::LinuxCheck
            | Self::LinuxWasm
            | Self::LinuxTest
            | Self::LinuxDoc
            | Self::LinuxLoom
            | Self::LinuxBroadcast
            | Self::LinuxIntegrationRegressions
            | Self::LinuxSeleniumFirefox
            | Self::LinuxCoverage
            | Self::WebChromium
            | Self::WebFirefox
            | Self::WebSize
            | Self::DepsDeny
            | Self::DepsUnused
            | Self::DepsFeatures
            | Self::DepsSemver => CacheGroup::Linux,
            Self::WindowsArm64 | Self::WindowsX64 | Self::WindowsX64Build => CacheGroup::Windows,
            Self::AndroidBuild
            | Self::AndroidTest
            | Self::ReleaseAndroid
            | Self::ReleasePublish => CacheGroup::Host,
        }
    }

    pub(crate) const fn uses_sccache(self) -> bool {
        !matches!(self.cache_group(), CacheGroup::Windows)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum PipelineKind {
    Branch,
    Platforms,
    MergeRequest,
    Quarantine,
    Main,
    Nightly,
    Weekly,
    Release,
}

#[derive(Debug, Args)]
pub(crate) struct RunArgs {
    /// CI lane to execute.
    lane: Lane,
    /// Pipeline policy used by lanes with fast and full variants.
    #[arg(
        long,
        env = "KITHARA_PIPELINE_KIND",
        value_enum,
        default_value = "merge-request"
    )]
    kind: PipelineKind,
}

#[derive(Debug)]
struct LinuxImageAttestation {
    runner: String,
    commit: String,
    provisioned: Option<String>,
}

struct Consts;

impl Consts {
    const CONNECTION_REFUSED_CODE: Option<&str> = if cfg!(target_os = "macos") {
        Some("(os error 61)")
    } else if cfg!(target_os = "linux") {
        Some("(os error 111)")
    } else if cfg!(windows) {
        Some("(os error 10061)")
    } else {
        None
    };
    const SCCACHE_COMMAND_ERROR: i32 = 2;
    const SCCACHE_CONNECT_ERROR: &str = "sccache: error: couldn't connect to server";
    const SCCACHE_MISSING_UDS_CODE: Option<&str> = if cfg!(unix) {
        Some("(os error 2)")
    } else {
        None
    };
    const SCCACHE_STOP_MESSAGE: &str = "Stopping sccache server...";
}

impl LinuxImageAttestation {
    fn from_gitlab() -> Result<Option<Self>> {
        if !is_gitlab() {
            return Ok(None);
        }
        Ok(Some(Self {
            runner: env::var("CI_RUNNER_DESCRIPTION")
                .context("CI_RUNNER_DESCRIPTION must name the Linux executor")?,
            commit: env::var("CI_COMMIT_SHA").context("CI_COMMIT_SHA must name the CI checkout")?,
            provisioned: env::var(PROVISIONED_LINUX_IMAGE_ENV).ok(),
        }))
    }
}

fn linux_image_diagnosis(
    config: &CiConfig,
    runner: &str,
    commit: &str,
    provisioned: &str,
) -> String {
    let expected = &config.pins.linux_image;
    let host_config = config.host.host_root.join("services/mac-host.toml");
    format!(
        "Linux CI image `{expected}` is not provisioned on runner `{runner}`; the runner declares \
         `{provisioned}`. On the Mac mini that owns `{runner}`, log in as `kithara-ci`, check out \
         commit `{commit}`, and run from that checkout:\n\
         `cargo build --locked --release -p xtask`\n\
         `export KITHARA_CI_HOST_CONFIG={}`\n\
         `export KITHARA_CI_PINS=$PWD/.config/ci-pins.toml`\n\
         `target/release/xtask ci host build-linux-image $PWD/docker/ci.Dockerfile`\n\
         `target/release/xtask ci host configure-runners`\n\
         `target/release/xtask ci host activate`",
        host_config.display()
    )
}

fn require_provisioned_linux_image(
    lane: Lane,
    config: &CiConfig,
    attestation: Option<&LinuxImageAttestation>,
) -> Result<()> {
    if lane.cache_group() != CacheGroup::Linux {
        return Ok(());
    }
    let Some(attestation) = attestation else {
        return Ok(());
    };
    match attestation.provisioned.as_deref() {
        Some(provisioned) if provisioned == config.pins.linux_image => Ok(()),
        Some(provisioned) => bail!(
            "{}",
            linux_image_diagnosis(
                config,
                &attestation.runner,
                &attestation.commit,
                provisioned
            )
        ),
        None => bail!(
            "{}",
            linux_image_diagnosis(
                config,
                &attestation.runner,
                &attestation.commit,
                "not declared by runner",
            )
        ),
    }
}

fn sccache_server_is_stopped(code: Option<i32>, stdout: &[u8], stderr: &[u8]) -> bool {
    let stdout = String::from_utf8_lossy(stdout);
    let stderr = String::from_utf8_lossy(stderr);
    code == Some(Consts::SCCACHE_COMMAND_ERROR)
        && stdout.trim() == Consts::SCCACHE_STOP_MESSAGE
        && stderr.contains(Consts::SCCACHE_CONNECT_ERROR)
        && (Consts::CONNECTION_REFUSED_CODE.is_some_and(|code| stderr.contains(code))
            || Consts::SCCACHE_MISSING_UDS_CODE.is_some_and(|code| stderr.contains(code)))
}

fn sccache_server_already_stopped(output: &Output) -> bool {
    sccache_server_is_stopped(output.status.code(), &output.stdout, &output.stderr)
}

fn retire_sccache_server(process: &Process) -> Result<()> {
    process.ensure(
        "sccache",
        &["--stop-server"],
        "retire the compiler cache",
        sccache_server_already_stopped,
    )
}

fn execute_lane(
    process: &Process,
    uses_sccache: bool,
    has_server_uds: bool,
    dispatch: impl FnOnce() -> Result<()>,
) -> Result<()> {
    if uses_sccache {
        retire_sccache_server(process)?;
        if has_server_uds {
            process.run("sccache", &["--start-server"], "start the compiler cache")?;
        }
    }
    let result = dispatch();
    if uses_sccache {
        process.best_effort("sccache", &["--show-stats"], "sccache statistics");
    }
    result
}

pub(crate) fn run(args: &RunArgs, ctx: &Ctx) -> Result<()> {
    let lane_name = args
        .lane
        .to_possible_value()
        .map_or_else(|| "lane".to_owned(), |value| value.get_name().to_owned());
    verdict::clear(&ctx.root, &lane_name)?;
    let outcome = execute(args, ctx);
    // A lane that fell over before it reached its own work — no host profile,
    // no room on the cache volume — is still a lane that failed, and the
    // verdict has to hear about it. Collecting only after a dispatched lane
    // let exactly that go unrecorded on the first live run.
    if let Err(error) = verdict::gather(&ctx.root, &lane_name, outcome.is_err()) {
        warn!(%error, lane = %lane_name, "could not collect this lane's test report");
    }
    outcome
}

fn execute(args: &RunArgs, ctx: &Ctx) -> Result<()> {
    let ext = KitharaExt::from_ctx(ctx)?;
    ext.ci.validate()?;
    let host_config = env::var_os("KITHARA_CI_HOST_CONFIG")
        .map(PathBuf::from)
        .context(
            "KITHARA_CI_HOST_CONFIG must point at the host profile installed on this executor",
        )?;
    let ci_config = CiConfig::load(&host_config, &ctx.root.join(&ext.ci.pins))?;
    let image_attestation = LinuxImageAttestation::from_gitlab()?;
    require_provisioned_linux_image(args.lane, &ci_config, image_attestation.as_ref())?;
    let environment = CiEnvironment::prepare(ctx, &ci_config, args.lane)?;
    info!(
        lane = ?args.lane,
        kind = ?args.kind,
        cache_root = %environment.cache_root.display(),
        "CI environment prepared"
    );
    let swiftpm_cache = environment.swiftpm_cache.clone();
    let temp = environment.temp.clone();
    let uses_sccache = environment.uses_sccache();
    let vars = environment.vars();
    let has_server_uds = vars.contains_key(OsStr::new("SCCACHE_SERVER_UDS"));
    let process = Process::new(&ctx.root, vars);
    // sccache is a daemon, and a running one keeps the cache directory it was
    // started with — a client inherits the server's configuration, not its
    // own. An executor that outlives a job therefore carries the previous
    // job's cache location into this one, and a stale location that no longer
    // exists fails every compilation while reporting only that a C compiler
    // exited 254. Retire the server so it restarts with what this job asked
    // for; the cache on disk is untouched. Shared-host Unix sockets need one
    // explicit start before Cargo's parallel compilers can race to start it.
    execute_lane(&process, uses_sccache, has_server_uds, || match args.lane {
        Lane::AppleLint => lane::apple::lint(&process, &ci_config, args.kind),
        Lane::AppleMsrv => lane::apple::msrv(&process, &ci_config),
        Lane::AppleTest => lane::apple::test(&process, &ci_config, args.kind),
        Lane::AppleTestFlashOff => lane::apple::test_flash_off(&process, &ci_config),
        Lane::AppleE2e => lane::apple::e2e(&process, &ci_config),
        Lane::AppleXcframework => lane::apple::xcframework(&process, &ci_config),
        Lane::AppleSwiftTest => lane::apple::swift_test(&process, &ci_config, &swiftpm_cache),
        Lane::AppleIos => lane::apple::ios(&process, &ci_config),
        Lane::AppleIosTest => lane::apple::ios_test(&process, &ci_config),
        Lane::AppleSafari => lane::apple::safari(&process),
        Lane::LinuxSecrets => lane::linux::secrets(&process),
        Lane::LinuxCheck => lane::linux::check(&process),
        Lane::LinuxWasm => lane::linux::wasm(&process),
        Lane::LinuxTest => lane::linux::test(&process),
        Lane::LinuxDoc => lane::linux::configured(&process, "doc"),
        Lane::LinuxLoom => lane::linux::configured(&process, "loom"),
        Lane::LinuxBroadcast => lane::linux::configured(&process, "broadcast"),
        Lane::LinuxIntegrationRegressions => {
            lane::linux::configured(&process, "integration-regressions")
        }
        Lane::LinuxSeleniumFirefox => lane::linux::selenium(&process, "firefox"),
        Lane::LinuxCoverage => lane::linux::coverage(&process),
        Lane::AndroidBuild => lane::android::build(&process),
        Lane::AndroidTest => lane::android::test(&process, &ci_config),
        Lane::WebChromium => lane::web::chromium(&process, &ci_config.pins),
        Lane::WebFirefox => lane::web::firefox(&process),
        Lane::WebSize => lane::web::size(&process),
        Lane::WindowsArm64 => lane::windows::tests(&process, "aarch64-pc-windows-msvc"),
        Lane::WindowsX64 => lane::windows::tests(&process, "x86_64-pc-windows-msvc"),
        Lane::WindowsX64Build => lane::windows::build(&process, "x86_64-pc-windows-msvc"),
        Lane::DeepRtsan => lane::deep::rtsan(&process),
        Lane::DeepPerf => lane::deep::perf(&process),
        Lane::DeepBench => lane::deep::bench(&process),
        Lane::DepsDeny => lane::deps::deny(&process),
        Lane::DepsUnused => lane::deps::unused(&process),
        Lane::DepsFeatures => lane::deps::features(&process),
        Lane::DepsSemver => lane::deps::semver(&process),
        Lane::ReleaseXcframework => {
            super::release::xcframework(&process, ctx, &ext, &temp, args.kind)
        }
        Lane::ReleaseDocs => super::release::docs(&process, ctx, &ext),
        Lane::ReleaseWasm => super::release::wasm(&process, ctx, &ext),
        Lane::ReleaseAndroid => super::release::build_android(&process, ctx, &ext),
        Lane::ReleasePublish => super::release::publish(&process, ctx, &ext, args.kind),
        Lane::Verdict => verdict::lane(&ctx.root, environment.shared_root(), args.kind),
    })
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::{collections::BTreeMap, ffi::OsString, os::unix::fs::PermissionsExt};
    use std::{collections::BTreeSet, fs, path::Path};

    use clap::{Parser, ValueEnum};

    #[cfg(unix)]
    use super::execute_lane;
    use super::{
        CacheGroup, Consts, Lane, LinuxImageAttestation, linux_image_diagnosis,
        require_provisioned_linux_image, sccache_server_is_stopped,
    };
    use crate::Cli;
    #[cfg(unix)]
    use crate::ci::process::Process;

    /// Lane names cross a language boundary: the enum is Rust, the schedule is
    /// pipeline YAML, and nothing but this test connects them. Renaming a lane
    /// without renaming its job used to fail in CI, on the runner, minutes in.
    fn scheduled_lanes() -> BTreeSet<String> {
        let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("xtask has a workspace root")
            .join(".gitlab/ci");
        let mut lanes = BTreeSet::new();
        for entry in fs::read_dir(&directory).expect("pipeline directory is readable") {
            let path = entry.expect("pipeline entry is readable").path();
            let text = fs::read_to_string(&path).expect("pipeline file is UTF-8");
            lanes.extend(
                text.split("ci run ")
                    .skip(1)
                    .filter_map(|rest| rest.split_whitespace().next())
                    .map(str::to_string),
            );
        }
        lanes
    }

    #[cfg(unix)]
    #[test]
    fn sccache_lifecycle_precedes_real_lane_dispatch() {
        const UDS_READY: &str = "--stop-server\n--start-server\nlane\n--show-stats\n";
        const NON_UDS: &str = "--stop-server\nlane\n--show-stats\n";

        let directory = tempfile::tempdir().unwrap();
        let bin = directory.path().join("bin");
        fs::create_dir(&bin).unwrap();
        let sccache = bin.join("sccache");
        fs::write(
            &sccache,
            r#"#!/bin/sh
printf '%s\n' "$1" >> "$KITHARA_TEST_TRACE"
case "$1:$KITHARA_TEST_SCENARIO" in
    --stop-server:already-stopped)
        printf 'Stopping sccache server...\n'
        printf "%s\n" "sccache: error: couldn't connect to server" \
            "sccache: caused by: No such file or directory (os error 2)" >&2
        exit 2
        ;;
    --stop-server:stop-failure) exit 7 ;;
    --start-server:start-failure) exit 8 ;;
    --stop-server:*|--start-server:*|--show-stats:*) exit 0 ;;
esac
exit 19
"#,
        )
        .unwrap();
        let mut permissions = fs::metadata(&sccache).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&sccache, permissions).unwrap();
        let trace = directory.path().join("trace");

        for (scenario, uses_sccache, has_server_uds, expected_trace) in [
            ("success", true, true, UDS_READY),
            ("already-stopped", true, true, UDS_READY),
            ("stop-failure", true, true, "--stop-server\n"),
            (
                "start-failure",
                true,
                true,
                "--stop-server\n--start-server\n",
            ),
            ("success", true, false, NON_UDS),
            ("success", false, false, "lane\n"),
        ] {
            let _ = fs::remove_file(&trace);
            let process = Process::new(
                directory.path(),
                BTreeMap::from([
                    (OsString::from("PATH"), bin.clone().into_os_string()),
                    (
                        OsString::from("KITHARA_TEST_SCENARIO"),
                        OsString::from(scenario),
                    ),
                    (
                        OsString::from("KITHARA_TEST_TRACE"),
                        trace.clone().into_os_string(),
                    ),
                ]),
            );

            let outcome = execute_lane(&process, uses_sccache, has_server_uds, || {
                let mut sequence = fs::read_to_string(&trace).unwrap_or_default();
                sequence.push_str("lane\n");
                fs::write(&trace, sequence)?;
                Ok(())
            });

            let succeeds = !scenario.ends_with("failure");
            assert_eq!(outcome.is_ok(), succeeds, "{scenario}: {outcome:?}");
            assert_eq!(
                fs::read_to_string(&trace).unwrap(),
                expected_trace,
                "{scenario}"
            );
        }
    }

    #[test]
    fn the_linux_pipeline_uses_the_runner_provisioned_image() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("xtask has a workspace root");
        let common = fs::read_to_string(root.join(".gitlab/ci/common.yml"))
            .expect("the shared pipeline definition is readable");
        let (_, after_linux) = common
            .split_once(".linux-job:")
            .expect("the Linux job template exists");
        let linux_job = after_linux
            .split_once("\n.")
            .map_or(after_linux, |(job, _)| job);
        assert!(
            linux_job
                .lines()
                .all(|line| !line.trim().starts_with("image:")),
            "the pipeline must not override the local image provisioned in runner config"
        );
    }

    #[test]
    fn verdict_uses_the_macos_cache_group() {
        assert_eq!(Lane::Verdict.cache_group(), CacheGroup::Macos);
    }

    #[test]
    fn windows_lanes_disable_sccache() {
        assert!(!Lane::WindowsX64.uses_sccache());
        assert!(Lane::LinuxCheck.uses_sccache());
    }

    #[test]
    fn manual_platform_dispatch_has_its_own_pipeline_kind() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("xtask has a workspace root");
        let pipeline = fs::read_to_string(root.join(".gitlab-ci.yml"))
            .expect("the parent pipeline definition is readable");
        let (_, after_platforms) = pipeline
            .split_once("dispatch:platforms:")
            .expect("the platform dispatcher exists");
        let (platforms, _) = after_platforms
            .split_once("dispatch:main:")
            .expect("the main dispatcher follows the platform dispatcher");

        assert!(platforms.contains("KITHARA_PIPELINE_KIND: platforms"));
        assert!(!platforms.contains("KITHARA_PIPELINE_KIND: main"));
    }

    #[test]
    fn platform_runs_schedule_verification_and_integration_lanes() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("xtask has a workspace root");
        let common = fs::read_to_string(root.join(".gitlab/ci/common.yml"))
            .expect("the shared pipeline definition is readable");

        for (name, next) in [
            (".rules-verify:", ".rules-integration:"),
            (".rules-integration:", ".rules-verify-and-branch:"),
        ] {
            let (_, after_rules) = common.split_once(name).expect("the rule set exists");
            let (rules, _) = after_rules
                .split_once(next)
                .expect("the next rule set exists");
            assert!(
                rules.contains("$KITHARA_PIPELINE_KIND == \"platforms\""),
                "{name} does not schedule a platform run"
            );
        }
    }

    #[test]
    fn linux_tests_do_not_disappear_when_linux_check_fails() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("xtask has a workspace root");
        let linux = fs::read_to_string(root.join(".gitlab/ci/linux.yml"))
            .expect("the Linux pipeline definition is readable");
        let (_, after_test) = linux
            .split_once("linux:test:")
            .expect("the Linux test job exists");
        let (test_job, coverage_job) = after_test
            .split_once("linux:coverage:")
            .expect("the coverage job follows Linux test");

        for (name, job) in [("linux:test", test_job), ("linux:coverage", coverage_job)] {
            assert!(job.contains("needs: []"), "{name} waits on an earlier job");
            assert!(
                !job.contains("linux:check"),
                "{name} disappears when linux:check fails"
            );
        }
    }

    #[test]
    fn ci_junit_paths_match_their_producers() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("xtask has a workspace root");
        let nextest: toml::Value = toml::from_str(
            &fs::read_to_string(root.join(".config/nextest.toml"))
                .expect("nextest configuration is readable"),
        )
        .expect("nextest configuration is TOML");
        assert_eq!(
            nextest["profile"]["ci"]["junit"]["path"].as_str(),
            Some("junit.xml")
        );

        let linux = fs::read_to_string(root.join(".gitlab/ci/linux.yml"))
            .expect("the Linux pipeline definition is readable");
        let (_, after_test) = linux
            .split_once("linux:test:")
            .expect("the Linux test job exists");
        let (test_job, _) = after_test
            .split_once("linux:coverage:")
            .expect("the coverage job follows Linux test");
        assert!(
            test_job
                .lines()
                .any(|line| line.trim() == "junit: target/nextest/ci/junit.xml"),
            "linux:test does not publish the nextest JUnit report"
        );

        let mut reports = Vec::new();
        for name in ["apple.yml", "linux.yml", "windows.yml"] {
            let pipeline = fs::read_to_string(root.join(".gitlab/ci").join(name))
                .expect("pipeline definition is readable");
            reports.extend(
                pipeline
                    .lines()
                    .filter_map(|line| line.trim().strip_prefix("junit: "))
                    .map(str::to_owned),
            );
        }
        assert!(
            !reports.is_empty(),
            "the pipeline declares no JUnit reports"
        );
        reports.sort();
        assert_eq!(
            reports,
            [
                "target-flash-off/nextest/ci/junit.xml",
                "target/nextest/ci/junit.xml",
                "target/nextest/ci/junit.xml",
                "target/nextest/ci/junit.xml",
                "target/nextest/ci/junit.xml",
                "target/xcresult/ios-test.junit.xml",
                "target/xcresult/swift-test.junit.xml",
            ]
        );
    }

    #[test]
    fn image_drift_diagnosis_names_the_image_host_and_build_command() {
        let config = crate::ci::config::fixture();
        let diagnosis = linux_image_diagnosis(
            &config,
            "kithara-mac-mini-linux",
            "0123456789abcdef",
            "kithara-ci:old",
        );

        assert!(diagnosis.contains(&config.pins.linux_image));
        assert!(diagnosis.contains("kithara-mac-mini-linux"));
        assert!(diagnosis.contains("0123456789abcdef"));
        assert!(diagnosis.contains("kithara-ci:old"));
        assert!(
            diagnosis.contains(
                "target/release/xtask ci host build-linux-image $PWD/docker/ci.Dockerfile"
            )
        );
    }

    #[test]
    fn a_local_linux_lane_does_not_require_runner_attestation() {
        let config = crate::ci::config::fixture();

        require_provisioned_linux_image(Lane::LinuxCheck, &config, None).unwrap();
    }

    fn image_attestation(provisioned: Option<&str>) -> LinuxImageAttestation {
        LinuxImageAttestation {
            runner: "kithara-mac-mini-linux".to_owned(),
            commit: "0123456789abcdef".to_owned(),
            provisioned: provisioned.map(str::to_owned),
        }
    }

    #[test]
    fn a_gitlab_linux_lane_accepts_the_provisioned_image() {
        let config = crate::ci::config::fixture();
        let attestation = image_attestation(Some(&config.pins.linux_image));

        require_provisioned_linux_image(Lane::LinuxCheck, &config, Some(&attestation)).unwrap();
    }

    #[test]
    fn a_gitlab_linux_lane_rejects_a_different_image() {
        let config = crate::ci::config::fixture();
        let attestation = image_attestation(Some("kithara-ci:old"));

        let error = require_provisioned_linux_image(Lane::LinuxCheck, &config, Some(&attestation))
            .unwrap_err();

        assert!(error.to_string().contains("kithara-ci:old"));
    }

    #[test]
    fn a_gitlab_linux_lane_rejects_a_missing_image_declaration() {
        let config = crate::ci::config::fixture();
        let attestation = image_attestation(None);

        let error = require_provisioned_linux_image(Lane::LinuxCheck, &config, Some(&attestation))
            .unwrap_err();

        assert!(error.to_string().contains("not declared by runner"));
    }

    #[test]
    fn sccache_connection_refusal_is_an_already_stopped_state() {
        let Some(os_error) = Consts::CONNECTION_REFUSED_CODE else {
            return;
        };
        let stderr = format!(
            "sccache: error: couldn't connect to server\n\
             sccache: caused by: operating system refused the connection {os_error}"
        );

        assert!(sccache_server_is_stopped(
            Some(Consts::SCCACHE_COMMAND_ERROR),
            b"Stopping sccache server...\n",
            stderr.as_bytes(),
        ));
    }

    #[cfg(unix)]
    #[test]
    fn a_missing_sccache_uds_is_an_already_stopped_state() {
        assert!(sccache_server_is_stopped(
            Some(Consts::SCCACHE_COMMAND_ERROR),
            b"Stopping sccache server...\n",
            b"sccache: error: couldn't connect to server\n\
              sccache: caused by: No such file or directory (os error 2)",
        ));
    }

    #[test]
    fn sccache_rejects_another_platforms_connection_error() {
        let Some(expected) = Consts::CONNECTION_REFUSED_CODE else {
            return;
        };
        for os_error in ["(os error 61)", "(os error 111)", "(os error 10061)"] {
            if os_error == expected {
                continue;
            }
            let stderr = format!(
                "sccache: error: couldn't connect to server\n\
                 sccache: caused by: connection failed {os_error}"
            );
            assert!(!sccache_server_is_stopped(
                Some(Consts::SCCACHE_COMMAND_ERROR),
                b"Stopping sccache server...\n",
                stderr.as_bytes(),
            ));
        }
    }

    #[test]
    fn sccache_does_not_hide_other_exit_two_failures() {
        const UNRELATED_EXIT_CODE: i32 = 7;
        let Some(os_error) = Consts::CONNECTION_REFUSED_CODE else {
            return;
        };
        let refusal = format!(
            "sccache: error: couldn't connect to server\n\
             sccache: caused by: connection failed {os_error}"
        );

        assert!(!sccache_server_is_stopped(
            Some(UNRELATED_EXIT_CODE),
            b"Stopping sccache server...\n",
            refusal.as_bytes(),
        ));
        assert!(!sccache_server_is_stopped(
            Some(Consts::SCCACHE_COMMAND_ERROR),
            b"Unexpected command output\n",
            refusal.as_bytes(),
        ));
        let missing_header = format!("sccache: error: configuration is invalid {os_error}");
        assert!(!sccache_server_is_stopped(
            Some(Consts::SCCACHE_COMMAND_ERROR),
            b"Stopping sccache server...\n",
            missing_header.as_bytes(),
        ));
        assert!(!sccache_server_is_stopped(
            Some(Consts::SCCACHE_COMMAND_ERROR),
            b"Stopping sccache server...\n",
            b"sccache: error: couldn't connect to server\n\
              sccache: caused by: connection failed without an operating-system code",
        ));
    }

    #[test]
    fn ci_lane_is_typed_and_uses_modular_names() {
        assert!(Cli::try_parse_from(["xtask", "ci", "run", "apple-lint"]).is_ok());
        assert!(
            Cli::try_parse_from(["xtask", "ci", "run", "linux-test", "--kind", "quarantine"])
                .is_ok()
        );
        assert!(Cli::try_parse_from(["xtask", "ci", "run", "apple"]).is_err());
        assert!(Cli::try_parse_from(["xtask", "ci", "run", "unknown"]).is_err());
    }

    #[test]
    fn lanes_lease_the_cache_of_the_executor_they_run_on() {
        assert_eq!(Lane::ReleaseXcframework.cache_group(), CacheGroup::Macos);
        assert_eq!(Lane::WebFirefox.cache_group(), CacheGroup::Linux);
        assert_eq!(Lane::ReleaseAndroid.cache_group(), CacheGroup::Host);
        assert_eq!(Lane::WindowsX64Build.cache_group(), CacheGroup::Windows);
    }

    #[test]
    fn the_pipeline_schedules_every_lane_and_only_real_lanes() {
        let scheduled = scheduled_lanes();
        for name in &scheduled {
            assert!(
                Lane::from_str(name, false).is_ok(),
                "a pipeline job runs `{name}`, which is not a CI lane"
            );
        }
        for lane in Lane::value_variants() {
            let name = lane
                .to_possible_value()
                .expect("every lane is reachable from the command line");
            assert!(
                scheduled.contains(name.get_name()),
                "no pipeline job runs the {} lane",
                name.get_name()
            );
        }
    }
}

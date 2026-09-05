use std::{
    collections::BTreeMap,
    env,
    ffi::OsStr,
    path::{Path, PathBuf},
    process::Output,
};

use anyhow::{Context, Result, bail};
use clap::{Args, ValueEnum};
use kithara_devtools::{Ctx, common::tools::ToolsConfig};
use tracing::{info, warn};

use super::{
    config::CiConfig,
    environment::{CiEnvironment, PROVISIONED_LINUX_IMAGE_ENV, is_gitlab},
    process::{Process, Recording},
    verdict,
};
use crate::config::{CiLaneConfig, KitharaExt};

/// One CI job. Lanes are deliberately narrow: a job that does one thing can be
/// retried, skipped, or read on its own, and the step that failed is the job
/// name rather than a line buried in an hour of output. Where two lanes need
/// the same build, they repeat it — the executor keeps a warm target directory,
/// so repeating is cheaper than threading artifacts between jobs.
///
/// The variants left here are the ones that are programs rather than
/// invocations: they take the workspace context and produce artifacts, or hold
/// something open for the length of a run. Everything else a pipeline schedules
/// is `Declared`, and lives in `.config/xtask.toml`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Lane {
    AppleSwiftTest,
    AppleIosTest,
    ReleaseXcframework,
    ReleaseDocs,
    ReleaseWasm,
    ReleaseAndroid,
    ReleasePublish,
    Verdict,
    Declared {
        name: String,
        cache_group: CacheGroup,
    },
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

impl CacheGroup {
    pub(crate) const fn uses_sccache(self) -> bool {
        !matches!(self, Self::Windows)
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Macos => "macos",
            Self::Linux => "linux",
            Self::Windows => "windows",
            Self::Host => "host",
        }
    }

    fn parse(name: &str) -> Option<Self> {
        [Self::Macos, Self::Linux, Self::Windows, Self::Host]
            .into_iter()
            .find(|group| group.as_str() == name)
    }
}

impl Lane {
    /// The residual lanes, by the name a pipeline schedules them under. They
    /// are matched before the configuration is consulted, so a declared lane
    /// cannot shadow one of them.
    const RESIDUAL: [(&'static str, Self); 8] = [
        ("apple-swift-test", Self::AppleSwiftTest),
        ("apple-ios-test", Self::AppleIosTest),
        ("release-xcframework", Self::ReleaseXcframework),
        ("release-docs", Self::ReleaseDocs),
        ("release-wasm", Self::ReleaseWasm),
        ("release-android", Self::ReleaseAndroid),
        ("release-publish", Self::ReleasePublish),
        ("verdict", Self::Verdict),
    ];

    /// A lane name from the command line, against the lanes this repository
    /// actually has. An unknown one fails here rather than on the runner, and
    /// says what it could have been.
    pub(crate) fn parse(name: &str, lanes: &BTreeMap<String, CiLaneConfig>) -> Result<Self> {
        if let Some((_, lane)) = Self::RESIDUAL.iter().find(|(known, _)| *known == name) {
            return Ok(lane.clone());
        }
        if let Some(declared) = lanes.get(name) {
            let cache_group = CacheGroup::parse(&declared.cache_group)
                .with_context(|| format!("ext.ci.lanes.{name}.cache_group is not a cache group"))?;
            return Ok(Self::Declared {
                name: name.to_owned(),
                cache_group,
            });
        }
        bail!("`{name}` is not a CI lane; this repository has {}", {
            let mut names: Vec<&str> = Self::RESIDUAL.iter().map(|(known, _)| *known).collect();
            names.extend(lanes.keys().map(String::as_str));
            names.sort_unstable();
            names.join(", ")
        })
    }

    pub(crate) const fn cache_group(&self) -> CacheGroup {
        match self {
            Self::AppleSwiftTest
            | Self::AppleIosTest
            | Self::ReleaseXcframework
            | Self::ReleaseDocs
            | Self::ReleaseWasm
            | Self::Verdict => CacheGroup::Macos,
            Self::ReleaseAndroid | Self::ReleasePublish => CacheGroup::Host,
            Self::Declared { cache_group, .. } => *cache_group,
        }
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

impl PipelineKind {
    /// The name a pipeline sets `KITHARA_PIPELINE_KIND` to, which is also the
    /// name a lane's `kinds` entry uses. Spelled here rather than derived from
    /// clap so it borrows for the program's life; `lane_config.rs` pins it
    /// against `PIPELINE_KINDS`.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Branch => "branch",
            Self::Platforms => "platforms",
            Self::MergeRequest => "merge-request",
            Self::Quarantine => "quarantine",
            Self::Main => "main",
            Self::Nightly => "nightly",
            Self::Weekly => "weekly",
            Self::Release => "release",
        }
    }
}

#[derive(Debug, Args)]
pub(crate) struct RunArgs {
    /// CI lane to execute, as `.config/xtask.toml` and the pipeline name it.
    lane: String,
    /// Pipeline policy used by lanes with fast and full variants.
    #[arg(
        long,
        env = "KITHARA_PIPELINE_KIND",
        value_enum,
        default_value = "merge-request"
    )]
    kind: PipelineKind,
    /// Packaging profile from `ext.release.packages`. Defaults to the strict
    /// release path, so a caller that forgets one is not silently weakened.
    #[arg(long, default_value = "release")]
    package: String,
    /// Delivery channel from `ext.release.channels`.
    #[arg(long, default_value = "release")]
    channel: String,
    /// Report what the lane would require and run, without running it.
    #[arg(long)]
    dry_run: bool,
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
    cache_group: CacheGroup,
    config: &CiConfig,
    attestation: Option<&LinuxImageAttestation>,
) -> Result<()> {
    if cache_group != CacheGroup::Linux {
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

fn retire_sccache_server(process: &Process, tools: &ToolsConfig) -> Result<()> {
    process.ensure(
        tools.program("sccache"),
        &["--stop-server"],
        "retire the compiler cache",
        sccache_server_already_stopped,
    )
}

fn execute_lane(
    process: &Process,
    tools: &ToolsConfig,
    uses_sccache: bool,
    has_server_uds: bool,
    dispatch: impl FnOnce() -> Result<()>,
) -> Result<()> {
    if uses_sccache {
        retire_sccache_server(process, tools)?;
        if has_server_uds {
            process.run(
                tools.program("sccache"),
                &["--start-server"],
                "start the compiler cache",
            )?;
        }
    }
    let result = dispatch();
    if uses_sccache {
        process.best_effort(
            tools.program("sccache"),
            &["--show-stats"],
            "sccache statistics",
        );
    }
    result
}

pub(crate) fn run(args: &RunArgs, ctx: &Ctx) -> Result<()> {
    let lane_name = args.lane.clone();
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
    // The lane name is checked against this repository's lanes before anything
    // reads the executor, so a typo answers with the list of lanes rather than
    // with whatever the machine is missing.
    let lane = Lane::parse(&args.lane, &ext.ci.lanes)?;
    let host_config = env::var_os("KITHARA_CI_HOST_CONFIG")
        .map(PathBuf::from)
        .context(
            "KITHARA_CI_HOST_CONFIG must point at the host profile installed on this executor",
        )?;
    let ci_config = CiConfig::load(&host_config, &ctx.root.join(&ext.ci.pins))?;
    ci_config.pins.validate_tool_pins(&ctx.config.tools)?;
    let image_attestation = LinuxImageAttestation::from_gitlab()?;
    require_provisioned_linux_image(lane.cache_group(), &ci_config, image_attestation.as_ref())?;
    let environment = CiEnvironment::prepare(ctx, &ci_config, lane.cache_group())?;
    info!(
        lane = %args.lane,
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
    if args.dry_run {
        return report_lane(
            &lane,
            args.kind,
            &ctx.root,
            &ci_config,
            &ctx.config.tools,
            &swiftpm_cache,
            &ext.ci.lanes,
        );
    }
    execute_lane(
        &process,
        &ctx.config.tools,
        uses_sccache,
        has_server_uds,
        || match lane {
            Lane::ReleaseXcframework => {
                super::release::xcframework(&process, ctx, &ext, &temp, &args.package)
            }
            Lane::ReleaseDocs => super::release::docs(&process, ctx, &ext),
            Lane::ReleaseWasm => super::release::wasm(&process, ctx, &ext),
            Lane::ReleaseAndroid => super::release::build_android(&process, ctx, &ext),
            Lane::ReleasePublish => super::release::publish(&process, ctx, &ext, &args.channel),
            Lane::Verdict => verdict::lane(&ctx.root, environment.shared_root(), args.kind),
            ref lane => command_lane(
                lane,
                args.kind,
                &process,
                &ci_config,
                &ctx.config.tools,
                &swiftpm_cache,
                &ext.ci.lanes,
            ),
        },
    )
}

/// What the lane would ask of the executor, without asking. Answers "what does
/// this job actually run" from a laptop, and is what the lane snapshot reads.
fn report_lane(
    lane: &Lane,
    kind: PipelineKind,
    root: &Path,
    ci_config: &CiConfig,
    tools: &ToolsConfig,
    swiftpm_cache: &Path,
    lanes: &BTreeMap<String, CiLaneConfig>,
) -> Result<()> {
    let process = Process::recording(
        root,
        Recording::default()
            .with_reply(
                tools.program("xcodebuild"),
                &format!("Xcode {}", ci_config.pins.expected_xcode_version),
            )
            .with_reply("chromium", &ci_config.pins.chromium_version)
            .with_reply("chromedriver", &ci_config.pins.chromium_version),
    );
    let outcome = command_lane(lane, kind, &process, ci_config, tools, swiftpm_cache, lanes);
    let recorded = process
        .recorded()
        .context("a recording process keeps its recording")?;
    if let Err(error) = &outcome {
        println!("refused: {error}");
    }
    for step in recorded.steps() {
        println!("{} {}", step.program, step.args.join(" "));
    }
    outcome
}

/// Every lane that resolves to commands on the executor.
///
/// `release` and `verdict` are absent on purpose: they take the workspace
/// context and produce artifacts - checksums, a release tag, the verdict
/// journal - so they are programs rather than invocations, and they stay with
/// the caller.
fn command_lane(
    lane: &Lane,
    kind: PipelineKind,
    process: &Process,
    ci_config: &CiConfig,
    tools: &ToolsConfig,
    swiftpm_cache: &Path,
    lanes: &BTreeMap<String, CiLaneConfig>,
) -> Result<()> {
    match lane {
        Lane::ReleaseXcframework
        | Lane::ReleaseDocs
        | Lane::ReleaseWasm
        | Lane::ReleaseAndroid
        | Lane::ReleasePublish
        | Lane::Verdict => bail!("{lane:?} produces artifacts and is not a command lane"),
        Lane::AppleSwiftTest => {
            super::lane::apple::swift_test(process, ci_config, tools, swiftpm_cache)
        }
        Lane::AppleIosTest => super::lane::apple::ios_test(process, ci_config, tools),
        Lane::Declared { name, .. } => {
            let declared = lanes.get(name).with_context(|| {
                format!("ext.ci.lanes.{name} is not declared in .config/xtask.toml")
            })?;
            super::lane::declared::run(process, declared, &ci_config.pins, tools, kind)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{BTreeMap, BTreeSet},
        env,
        ffi::OsString,
        fs,
        path::Path,
    };

    use clap::{Parser, ValueEnum};
    use kithara_devtools::common::tools::ToolsConfig;
    use tempfile::TempDir;

    use super::{
        super::{config::fixture, process::Recording},
        CacheGroup, Consts, Lane, LinuxImageAttestation, PipelineKind, command_lane, execute_lane,
        linux_image_diagnosis, require_provisioned_linux_image, sccache_server_is_stopped,
    };
    use crate::{
        Cli,
        ci::process::Process,
        config::{CiLaneConfig, KitharaExt},
    };

    /// The repository the lane declarations are read from, whatever checkout a
    /// resolution then runs against.
    fn repo() -> &'static Path {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("xtask has a workspace root")
    }

    /// The checkout a lane resolves against. Routing asks the filesystem
    /// questions - `apple-ios-test` converts a result bundle only when a run
    /// left one behind - so resolving against the repository would make the
    /// answer depend on what an earlier build wrote into `target/`. An
    /// executor keeps that directory between jobs on purpose, so there the
    /// bundle is always present and the recorded answer never matched.
    fn checkout() -> TempDir {
        tempfile::tempdir().expect("create a resolution checkout")
    }

    /// The same checkout once a simulator run has left its result bundle, which
    /// is the state the conversion step exists for.
    fn checkout_after_a_simulator_run() -> TempDir {
        let checkout = checkout();
        fs::create_dir_all(checkout.path().join("target/xcresult/ios-test.xcresult"))
            .expect("leave a result bundle behind");
        checkout
    }

    /// One lane resolved in one pipeline kind: the program and arguments of
    /// each step it asks for, or the reason it refuses the kind.
    fn resolve(
        name: &str,
        kind: PipelineKind,
    ) -> (Result<(), String>, Vec<(String, Vec<String>, String)>) {
        let checkout = checkout();
        let root = checkout.path();
        let ci_config = fixture();
        let ext = KitharaExt::load(repo()).expect("the project config parses");
        let project = kithara_devtools::common::project::ProjectConfig::load(repo())
            .expect("the project config loads");
        let lane = Lane::parse(name, &ext.ci.lanes).expect("the lane is declared");
        let recording = Recording::default().with_reply(
            project.tools.program("xcodebuild"),
            &format!("Xcode {}", ci_config.pins.expected_xcode_version),
        );
        let process = Process::recording(root, recording);
        let outcome = command_lane(
            &lane,
            kind,
            &process,
            &ci_config,
            &project.tools,
            &root.join("target/swiftpm"),
            &ext.ci.lanes,
        )
        .map_err(|error| error.to_string());
        let steps = process
            .recorded()
            .expect("a recording process records")
            .steps()
            .iter()
            .map(|step| (step.program.clone(), step.args.clone(), step.label.clone()))
            .collect();
        (outcome, steps)
    }

    /// The step a lane exists to run, as opposed to the version probes and
    /// preflight it runs first.
    fn gate(name: &str, kind: PipelineKind) -> (Vec<String>, String) {
        let (outcome, steps) = resolve(name, kind);
        outcome.expect("the lane accepts this kind");
        let (_, args, label) = steps
            .into_iter()
            .find(|(program, _, _)| program == "just")
            .expect("the lane runs a just recipe");
        (args, label)
    }

    #[test]
    fn reviewed_pipelines_run_the_explicit_flash_and_no_block_gate() {
        for kind in [
            PipelineKind::MergeRequest,
            PipelineKind::Branch,
            PipelineKind::Main,
            PipelineKind::Platforms,
            PipelineKind::Nightly,
            PipelineKind::Release,
        ] {
            let (args, _) = gate("apple-test", kind);

            assert_eq!(
                args,
                [
                    "test",
                    "run",
                    "--flash=on",
                    "--no-block=on",
                    "--profile",
                    "ci"
                ],
                "{kind:?} must run the explicit gate"
            );
        }
    }

    #[test]
    fn quarantine_keeps_its_plain_profile_probe() {
        let (args, _) = gate("apple-test", PipelineKind::Quarantine);

        assert_eq!(args, ["test", "run", "--profile", "ci"]);
    }

    #[test]
    fn weekly_pipelines_do_not_claim_to_run_the_apple_suite() {
        let (outcome, _) = resolve("apple-test", PipelineKind::Weekly);

        assert_eq!(
            outcome.unwrap_err(),
            "weekly pipelines do not run the Apple suite"
        );
    }

    /// Saying that a lane does not run on this pipeline is an answer about the
    /// pipeline, so it costs nothing the machine has to provide: no platform
    /// check, no tool lookup, no version probe.
    #[test]
    fn a_refused_kind_asks_the_machine_for_nothing() {
        let (_, steps) = resolve("apple-test", PipelineKind::Weekly);

        assert_eq!(steps, Vec::new());
    }

    #[test]
    fn platform_runs_use_the_default_branch_apple_lint_gate() {
        let (args, label) = gate("apple-lint", PipelineKind::Platforms);

        assert_eq!(args, ["lint", "full"]);
        assert_eq!(label, "full lint gate");
    }

    /// The conversion runs off what a simulator run leaves behind, so a
    /// checkout without a result bundle asks `xcrun` for nothing. The snapshot
    /// resolves the same lane in a checkout that has one.
    #[test]
    fn a_checkout_without_a_result_bundle_converts_nothing() {
        let (outcome, steps) = resolve("apple-ios-test", PipelineKind::Main);

        assert_eq!(outcome, Ok(()));
        assert!(steps.iter().all(|(program, ..)| program != "xcrun"));
    }

    /// Every command lane, resolved but not run, in every pipeline kind it
    /// accepts. The snapshot is taken from the code that owns the routing
    /// today; moving that routing into configuration has to reproduce it
    /// byte for byte, which is a check that does not depend on CI - and CI is
    /// the one thing a routing change cannot be verified against.
    #[test]
    fn every_command_lane_resolves_to_its_recorded_commands() {
        let ci_config = fixture();
        let ext = KitharaExt::load(repo()).expect("the project config parses");
        let project = kithara_devtools::common::project::ProjectConfig::load(repo())
            .expect("the project config loads");
        let checkout = checkout_after_a_simulator_run();
        let root = checkout.path();
        let mut report = String::new();

        for name in every_lane() {
            let lane = Lane::parse(&name, &ext.ci.lanes).expect("every lane parses");
            for kind in PipelineKind::value_variants() {
                let kind_name = kind
                    .to_possible_value()
                    .expect("every kind has a name")
                    .get_name()
                    .to_owned();
                let version = ci_config.pins.chromium_version.clone();
                let recording = Recording::default()
                    .with_reply(
                        project.tools.program("xcodebuild"),
                        &format!("Xcode {}", ci_config.pins.expected_xcode_version),
                    )
                    .with_reply("chromium", &version)
                    .with_reply("chromedriver", &version)
                    // Enough for the conversion to resolve; what it makes of
                    // real results is pinned where that conversion lives.
                    .with_reply("xcrun", r#"{"testNodes":[]}"#);
                let process = Process::recording(root, recording);
                let outcome = command_lane(
                    &lane,
                    *kind,
                    &process,
                    &ci_config,
                    &project.tools,
                    &root.join("target/swiftpm"),
                    &ext.ci.lanes,
                );
                let steps = process.recorded().expect("a recording process records");
                report.push_str(&format!("# {name} / {kind_name}\n"));
                if let Err(error) = outcome {
                    report.push_str(&format!("  refused: {error}\n"));
                }
                for step in steps.steps() {
                    report.push_str(&format!("  {}", step.program));
                    if !step.args.is_empty() {
                        report.push_str(&format!(" {}", step.args.join(" ")));
                    }
                    report.push('\n');
                    // Several lanes differ from each other by nothing but their
                    // environment - a build-job cap, a target directory, a
                    // toolchain - so a snapshot that recorded only the command
                    // would let exactly those differences drift unseen.
                    for (key, value) in &step.env {
                        report.push_str(&format!("    {key}={value}\n"));
                    }
                    if !step.relative_dir.is_empty() {
                        report.push_str(&format!("    <in> {}\n", step.relative_dir));
                    }
                }
            }
        }

        let report = portable(&report, root);
        let snapshot =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ci-lane-commands.txt");
        if env::var_os("KITHARA_UPDATE_SNAPSHOT").is_some() {
            fs::write(&snapshot, &report).expect("snapshot is writable");
        }
        let expected = fs::read_to_string(&snapshot).expect("the lane snapshot exists");
        assert_eq!(
            report, expected,
            "a lane resolves to a different command than the snapshot records; \
             re-record with KITHARA_UPDATE_SNAPSHOT=1 only when the change is intended"
        );
    }

    /// Two things a lane resolves to are true only of the machine that resolved
    /// them: the checkout it sits in, and the test binary Cargo happened to
    /// build. Both are named rather than spelled out, so the snapshot says the
    /// same thing on a runner as it does on a laptop.
    fn portable(report: &str, root: &Path) -> String {
        let report = env::current_exe().map_or_else(
            |_| report.to_owned(),
            |exe| report.replace(&exe.display().to_string(), "<xtask>"),
        );
        report.replace(&root.display().to_string(), "<root>")
    }

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

    #[test]
    fn sccache_lifecycle_precedes_real_lane_dispatch() {
        const UDS_READY: &str = "--stop-server\n--start-server\nlane\n--show-stats\n";
        const NON_UDS: &str = "--stop-server\nlane\n--show-stats\n";

        let directory = tempfile::tempdir().unwrap();
        let bin = directory.path().join("bin");
        super::super::host::testing::install_double(&bin, "sccache");
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
            let mut vars = BTreeMap::from([
                (OsString::from("PATH"), bin.clone().into_os_string()),
                (
                    OsString::from("KITHARA_TEST_SCENARIO"),
                    OsString::from(scenario),
                ),
                (
                    OsString::from("KITHARA_TEST_TRACE"),
                    trace.clone().into_os_string(),
                ),
                (
                    OsString::from("KITHARA_TEST_RULES"),
                    OsString::from(
                        "sccache:--stop-server:stop-failure=7,\
                         sccache:--start-server:start-failure=8,\
                         sccache:--stop-server:already-stopped=2",
                    ),
                ),
            ]);
            if scenario == "already-stopped" {
                let refused = Consts::SCCACHE_MISSING_UDS_CODE
                    .or(Consts::CONNECTION_REFUSED_CODE)
                    .expect("this platform names an error code for a server that is not listening");
                vars.insert(
                    OsString::from("KITHARA_TEST_STDOUT"),
                    OsString::from("Stopping sccache server...\n"),
                );
                vars.insert(
                    OsString::from("KITHARA_TEST_STDERR"),
                    OsString::from(format!(
                        "sccache: error: couldn't connect to server\n\
                         sccache: caused by: no server was listening {refused}\n"
                    )),
                );
            }
            let process = Process::new(directory.path(), vars);

            let outcome = execute_lane(
                &process,
                &ToolsConfig::default(),
                uses_sccache,
                has_server_uds,
                || {
                    let mut sequence = fs::read_to_string(&trace).unwrap_or_default();
                    sequence.push_str("lane\n");
                    fs::write(&trace, sequence)?;
                    Ok(())
                },
            );

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
        assert!(!CacheGroup::Windows.uses_sccache());
        assert!(CacheGroup::Linux.uses_sccache());
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
            test_job.lines().any(|line| {
                line.trim() == "junit: .ci-artifacts/junit/linux-test-simulated-clock.xml"
            }),
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
                ".ci-artifacts/junit/apple-test-flash-off.xml",
                ".ci-artifacts/junit/apple-test.xml",
                ".ci-artifacts/junit/linux-test-simulated-clock.xml",
                "target/nextest/ci/junit.xml",
                "target/nextest/ci/junit.xml",
                "target/xcresult/ios-test.junit.xml",
                "target/xcresult/swift-test.junit.xml",
            ]
        );
    }

    #[test]
    fn image_drift_diagnosis_names_the_image_host_and_build_command() {
        let config = fixture();
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
        let config = fixture();

        require_provisioned_linux_image(CacheGroup::Linux, &config, None).unwrap();
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
        let config = fixture();
        let attestation = image_attestation(Some(&config.pins.linux_image));

        require_provisioned_linux_image(CacheGroup::Linux, &config, Some(&attestation)).unwrap();
    }

    #[test]
    fn a_gitlab_linux_lane_rejects_a_different_image() {
        let config = fixture();
        let attestation = image_attestation(Some("kithara-ci:old"));

        let error = require_provisioned_linux_image(CacheGroup::Linux, &config, Some(&attestation))
            .unwrap_err();

        assert!(error.to_string().contains("kithara-ci:old"));
    }

    #[test]
    fn a_gitlab_linux_lane_rejects_a_missing_image_declaration() {
        let config = fixture();
        let attestation = image_attestation(None);

        let error = require_provisioned_linux_image(CacheGroup::Linux, &config, Some(&attestation))
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

    /// The lane name is no longer a Rust type, so the check that it names
    /// something moved from clap to `Lane::parse` - and it happens before the
    /// executor is touched, with the list of names it could have been.
    #[test]
    fn an_unknown_lane_fails_with_the_names_it_could_have_been() {
        assert!(Cli::try_parse_from(["xtask", "ci", "run", "apple-lint"]).is_ok());
        assert!(
            Cli::try_parse_from([
                "xtask",
                "ci",
                "run",
                "linux-test-simulated-clock",
                "--kind",
                "quarantine"
            ])
            .is_ok()
        );

        let lanes = declared_lanes();
        assert!(Lane::parse("linux-test-simulated-clock", &lanes).is_ok());
        assert!(Lane::parse("verdict", &lanes).is_ok());
        let error = Lane::parse("apple", &lanes).unwrap_err().to_string();
        assert!(error.starts_with("`apple` is not a CI lane"), "{error}");
        assert!(error.contains("apple-lint"), "{error}");
    }

    #[test]
    fn lanes_lease_the_cache_of_the_executor_they_run_on() {
        let lanes = declared_lanes();

        assert_eq!(Lane::ReleaseXcframework.cache_group(), CacheGroup::Macos);
        assert_eq!(Lane::ReleaseAndroid.cache_group(), CacheGroup::Host);
        assert_eq!(
            Lane::parse("web-firefox", &lanes).unwrap().cache_group(),
            CacheGroup::Linux
        );
        assert_eq!(
            Lane::parse("windows-x64-build", &lanes)
                .unwrap()
                .cache_group(),
            CacheGroup::Windows
        );
    }

    fn declared_lanes() -> BTreeMap<String, CiLaneConfig> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("xtask has a workspace root");
        KitharaExt::load(root)
            .expect("the project config parses")
            .ci
            .lanes
    }

    /// Every lane this repository has, under the name a pipeline schedules it
    /// by: the ones that are still Rust, and the ones that are configuration.
    fn every_lane() -> Vec<String> {
        let mut names: Vec<String> = Lane::RESIDUAL
            .iter()
            .map(|(name, _)| (*name).to_owned())
            .collect();
        names.extend(declared_lanes().keys().cloned());
        names.sort();
        names
    }

    /// Lanes no pipeline schedules and no fleet claims. Each one is reached by
    /// name alone and says so with empty membership, which is a declaration
    /// rather than an oversight - and naming them here is what keeps a lane
    /// that merely forgot its membership from hiding among them.
    const BY_NAME_ONLY: [&str; 1] = ["deep-ui"];

    #[test]
    fn a_pipeline_job_only_ever_runs_a_declared_lane() {
        let lanes = declared_lanes();
        for name in scheduled_lanes() {
            assert!(
                Lane::parse(&name, &lanes).is_ok(),
                "a pipeline job runs `{name}`, which is not a CI lane"
            );
        }
    }

    /// `kinds` is GitLab's membership and `kinds_github` is GitHub's, so a
    /// lane claiming a pipeline kind here is claiming a GitLab job runs it.
    #[test]
    fn a_lane_that_claims_a_pipeline_kind_has_a_pipeline_job() {
        let scheduled = scheduled_lanes();
        for (name, lane) in declared_lanes() {
            if lane.kinds.is_empty() {
                continue;
            }
            assert!(
                scheduled.contains(&name),
                "the {name} lane claims {:?}, but no pipeline job runs it",
                lane.kinds
            );
        }
    }

    /// The lanes that are still Rust are GitLab's by construction, so this
    /// only has to answer for the declared ones: a lane belongs to a fleet, or
    /// it belongs to the list above.
    #[test]
    fn every_declared_lane_is_claimed_by_a_fleet_or_named_as_by_name_only() {
        let scheduled = scheduled_lanes();
        for (name, lane) in declared_lanes() {
            assert!(
                scheduled.contains(&name)
                    || !lane.kinds_github.is_empty()
                    || BY_NAME_ONLY.contains(&name.as_str()),
                "nothing reaches the {name} lane: no pipeline job runs it, it claims no \
                 GitHub pipeline kind, and it is not named as reachable by name alone"
            );
        }
    }
}

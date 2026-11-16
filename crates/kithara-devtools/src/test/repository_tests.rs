use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::PathBuf,
    process::Command,
};

use cargo_metadata::{Metadata, MetadataCommand};
use tempfile::TempDir;

use super::{NextestAction, request::TestRequest, resolve, selection::requested};
use crate::common::project::{ProjectConfig, TestCargoOptions, TestRunner};

pub(crate) fn root() -> PathBuf {
    crate::testing::package().join("../..")
}

#[test]
fn repository_readers_follow_the_running_checkout() {
    let checkout = tempfile::tempdir().expect("relocated checkout");
    let package = checkout.path().join("crates/kithara-devtools");
    fs::create_dir_all(&package).expect("relocated package");
    fs::create_dir(checkout.path().join(".config")).expect("relocated configuration");
    fs::write(
        checkout.path().join(".config/xtask.toml"),
        "[project]\nname = 'relocated'\n",
    )
    .expect("relocated project policy");
    let output = Command::new(std::env::current_exe().expect("current test binary"))
        .args(crate::common::child_test_args(
            module_path!(),
            "read_relocated_project",
        ))
        .env("CARGO_MANIFEST_DIR", package)
        .output()
        .expect("run configuration reader in relocated package");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.contains("running 1 test"),
        "configuration reader did not execute: {stdout} {stderr}"
    );
    assert!(
        output.status.success(),
        "configuration reader used another checkout: {stdout} {stderr}"
    );
}

#[test]
#[ignore = "executed by the relocation subprocess"]
fn read_relocated_project() {
    let config = ProjectConfig::load(&root()).expect("executing checkout configuration");
    assert_eq!(config.project.name, "relocated");
}

pub(crate) fn this_workspace() -> Metadata {
    MetadataCommand::new()
        .manifest_path(root().join("Cargo.toml"))
        .no_deps()
        .exec()
        .expect("cargo metadata for this workspace")
}

#[test]
fn engine_trace_consumers_do_not_build_devtools_commands() {
    let output = Command::new("cargo")
        .current_dir(root())
        .env("CARGO_TERM_COLOR", "always")
        .args([
            "tree",
            "--color",
            "never",
            "--locked",
            "--offline",
            "-p",
            "kithara-integration-tests",
            "-p",
            "kithara-queue-tests",
            "--features",
            "kithara-integration-tests/all",
            "--target",
            "x86_64-unknown-linux-gnu",
            "--edges",
            "normal,build",
            "--prefix",
            "none",
            "--format",
            "{p}",
        ])
        .output()
        .expect("resolve engine dependency features");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let tree = String::from_utf8(output.stdout).expect("Cargo dependency tree is UTF-8");
    let packages = tree
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .collect::<BTreeSet<_>>();

    assert!(
        !packages.contains("kithara-devtools"),
        "engine trace consumers must not compile the command crate"
    );
    assert!(packages.contains("kithara-devtools-trace-format"));
}

/// The members a lane's cargo options select.
pub(crate) fn selected_by<'a>(
    cargo: &TestCargoOptions,
    metadata: &'a Metadata,
) -> BTreeSet<&'a str> {
    metadata
        .workspace_packages()
        .into_iter()
        .map(|package| package.name.as_str())
        .filter(|name| {
            if cargo.workspace {
                !cargo.exclude.iter().any(|excluded| excluded == name)
            } else {
                cargo.packages.iter().any(|package| package == name)
            }
        })
        .collect()
}

/// `selected` and every member it reaches through path dependencies, which
/// is what cargo builds for the selection.
fn build_closure<'a>(selected: &BTreeSet<&'a str>, metadata: &'a Metadata) -> BTreeSet<&'a str> {
    let members = metadata
        .workspace_packages()
        .into_iter()
        .map(|package| (package.name.as_str(), package))
        .collect::<BTreeMap<_, _>>();
    let mut built = selected.clone();
    let mut pending = selected.iter().copied().collect::<Vec<_>>();
    while let Some(name) = pending.pop() {
        for dependency in &members[name].dependencies {
            if dependency.path.is_some()
                && let Some((member, _)) = members.get_key_value(dependency.name.as_str())
                && built.insert(member)
            {
                pending.push(member);
            }
        }
    }
    built
}

/// Every feature a lane resolves to, at its defaults and with each toggle it
/// can carry requested on, is one its build declares: `pkg/feature` by a
/// member the lane builds, a bare feature by a member it selects, since
/// cargo applies a bare feature to the selected packages only.
#[test]
fn every_feature_a_lane_resolves_to_is_declared_by_its_build() {
    let metadata = this_workspace();
    let project = ProjectConfig::load(&root()).expect("load repository config");
    let test = &project.test;
    let declared = metadata
        .workspace_packages()
        .into_iter()
        .map(|package| (package.name.as_str(), &package.features))
        .collect::<BTreeMap<_, _>>();
    let toggled = TestRequest::parse(&[
        "--flash=on".to_owned(),
        "--no-block=on".to_owned(),
        "--load=on".to_owned(),
    ])
    .expect("parse request");
    let declares = |member: &str, name: &str| {
        declared
            .get(member)
            .is_some_and(|features| features.contains_key(name))
    };
    let mut failures = BTreeSet::new();
    for (lane_name, lane) in &test.lanes {
        let selected = selected_by(&lane.cargo, &metadata);
        let built = build_closure(&selected, &metadata);
        for request in [None, Some(&toggled)] {
            let choice = requested(test, lane_name, request).expect("lane");
            for feature in resolve(test, &choice).expect("resolve").features {
                let holds = match feature.split_once('/') {
                    Some((package, name)) => built.contains(package) && declares(package, name),
                    None => selected.iter().any(|member| declares(member, &feature)),
                };
                if !holds {
                    failures.insert(format!(
                        "lane `{lane_name}` resolves to `{feature}`, which its build does not declare"
                    ));
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{}",
        failures.into_iter().collect::<Vec<_>>().join("\n")
    );
}

/// A recorded `cargo test` call's options grouped flag by flag, `-p` spelled
/// as `--package`, in a fixed order.
fn flag_groups<S: AsRef<str>>(args: &[S]) -> Vec<(String, Option<String>)> {
    const VALUED: [&str; 6] = [
        "-p",
        "--package",
        "--exclude",
        "--test",
        "--features",
        "--profile",
    ];
    let mut groups = Vec::new();
    let mut iter = args.iter().map(AsRef::as_ref);
    while let Some(flag) = iter.next() {
        let value = VALUED
            .contains(&flag)
            .then(|| iter.next().map(str::to_owned))
            .flatten();
        let flag = if flag == "-p" { "--package" } else { flag };
        groups.push((flag.to_owned(), value));
    }
    groups.sort();
    groups
}

/// Contract 15: the pinned nextest asks `cargo test` to build exactly the
/// cargo arguments a lane derives, so the typed options are the build every
/// runner of the lane gets and the build the planner reads.
///
/// Each nextest lane lists through the pinned nextest with `CARGO` pointed at
/// a script that captures the first lane's real metadata and records its
/// calls. Every other lane must request the same graph to reuse that input.
#[cfg(unix)]
#[test]
fn the_pinned_nextest_builds_exactly_the_cargo_arguments_a_lane_derives() {
    use std::os::unix::fs::PermissionsExt;

    let root = root();
    let pins: toml::Table = toml::from_str(
        &fs::read_to_string(root.join(".config/ci-pins.toml")).expect("pins are readable"),
    )
    .expect("pins are TOML");
    let pin = pins["cargo_tools"]["cargo-nextest"]
        .as_str()
        .expect("nextest is pinned");
    let version = Command::new("cargo-nextest")
        .args(["nextest", "--version"])
        .output()
        .expect("cargo-nextest is on PATH");
    assert!(
        String::from_utf8_lossy(&version.stdout).contains(pin),
        "the contract is the pinned nextest's: install it with \
         `cargo install cargo-nextest --version {pin} --locked`"
    );
    let temp = TempDir::new().expect("temp dir");
    let real_cargo = MetadataCommand::new()
        .cargo_command()
        .get_program()
        .to_os_string();
    let graph = temp.path().join("metadata.json");
    let shim = temp.path().join("cargo");
    fs::write(
        &shim,
        "#!/bin/sh\n\
         if [ \"$1\" = metadata ] || [ \"$2\" = metadata ]; then\n\
           printf '%s\\n' \"$@\" > \"$SHIM_METADATA_LOG\"\n\
           if [ \"$SHIM_CAPTURE_METADATA\" = 1 ]; then\n\
             \"$REAL_CARGO\" \"$@\" > \"$SHIM_METADATA\" || exit \"$?\"\n\
           fi\n\
           exec cat \"$SHIM_METADATA\"\n\
         fi\n\
         printf '%s\\n' \"$@\" > \"$SHIM_LOG\"\n",
    )
    .expect("write the cargo shim");
    fs::set_permissions(&shim, fs::Permissions::from_mode(0o755)).expect("make the shim runnable");
    let project = ProjectConfig::load(&root).expect("load repository config");
    let test = &project.test;
    let lanes = test
        .lanes
        .iter()
        .filter(|(_, lane)| matches!(lane.runner, TestRunner::Nextest(_)))
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>();
    assert!(!lanes.is_empty(), "the repository configures nextest lanes");

    let mismatch = |lane: &str, capture: bool| {
        let resolved = resolve(test, &requested(test, lane, None).expect("lane")).expect("resolve");
        let command = resolved
            .command(NextestAction::List, &[])
            .expect("list command");
        let log = temp.path().join(format!("{lane}.args"));
        let output = Command::new("cargo-nextest")
            .args(command.get_args())
            .envs(
                command
                    .get_envs()
                    .filter_map(|(key, value)| value.map(|value| (key, value))),
            )
            .env("CARGO", &shim)
            .env("REAL_CARGO", &real_cargo)
            .env("SHIM_METADATA", &graph)
            .env("SHIM_CAPTURE_METADATA", if capture { "1" } else { "0" })
            .env(
                "SHIM_METADATA_LOG",
                temp.path().join(format!("{lane}.metadata-args")),
            )
            .env("SHIM_LOG", &log)
            .current_dir(&root)
            .output()
            .expect("run the pinned nextest");
        let Ok(recorded) = fs::read_to_string(&log) else {
            return Some(format!(
                "{lane}: nextest never called cargo test: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        };
        let args = recorded
            .lines()
            .filter(|arg| !arg.starts_with("--color"))
            .collect::<Vec<_>>();
        let build = [
            "test",
            "--no-run",
            "--message-format",
            "json-render-diagnostics",
        ];
        let Some(options) = args.strip_prefix(build.as_slice()) else {
            return Some(format!("{lane}: nextest asked cargo for {args:?}"));
        };
        let derived = resolved.cargo_args("--profile", false);
        (flag_groups(options) != flag_groups(&derived)).then(|| {
            format!(
                "{lane}: nextest asked cargo test for {options:?}; the lane derives {derived:?}"
            )
        })
    };
    assert_eq!(
        mismatch(lanes[0], true),
        None,
        "the first lane must capture the real metadata and match its build arguments"
    );
    let mismatch = &mismatch;
    let failures = std::thread::scope(|scope| {
        let workers = lanes[1..]
            .chunks(lanes.len().div_ceil(4))
            .map(|chunk| {
                scope.spawn(move || {
                    chunk
                        .iter()
                        .copied()
                        .filter_map(|lane| mismatch(lane, false))
                        .collect::<Vec<_>>()
                })
            })
            .collect::<Vec<_>>();
        workers
            .into_iter()
            .flat_map(|worker| worker.join().expect("a lane worker finishes"))
            .collect::<Vec<_>>()
    });

    assert!(failures.is_empty(), "{}", failures.join("\n"));
    let metadata_calls = lanes
        .iter()
        .map(|lane| {
            fs::read_to_string(temp.path().join(format!("{lane}.metadata-args")))
                .expect("every lane called cargo metadata")
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(
        metadata_calls.len(),
        1,
        "every lane must request the same metadata graph"
    );
    assert!(
        metadata_calls
            .iter()
            .all(|args| args.lines().any(|arg| arg == "--all-features")),
        "the pinned nextest must request the all-feature workspace graph"
    );
}

#[test]
fn the_pinned_nextest_rejects_an_earlier_failed_stress_iteration() {
    let fixture = crate::testing::package().join("tests/fixtures/nextest-stress");
    let temp = TempDir::new().expect("isolated stress fixture");
    for name in ["Cargo.toml", "stress.rs"] {
        fs::copy(fixture.join(name), temp.path().join(name)).expect("copy stress fixture");
    }
    let config = temp.path().join("nextest.toml");
    fs::write(&config, "[profile.default.junit]\npath = \"junit.xml\"\n")
        .expect("write isolated nextest configuration");
    let cargo_home = temp.path().join("cargo-home");
    fs::create_dir_all(cargo_home.join("bin")).expect("isolated Cargo-home tools");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let stale = cargo_home.join("bin/cargo-nextest");
        fs::write(
            &stale,
            "#!/bin/sh\nprintf '%s\\n' 'selected' > \"$CARGO_HOME/stale-selected\"\nprintf '%s\\n' 'stale Cargo-home nextest selected' >&2\nexit 0\n",
        )
        .expect("controlled stale Cargo-home executable");
        fs::set_permissions(stale, fs::Permissions::from_mode(0o755))
            .expect("stale Cargo-home executable permissions");
    }
    let lane = resolve::ResolvedLane {
        env: BTreeMap::from([("CARGO_HOME".to_owned(), cargo_home.display().to_string())]),
        backend: "fixture".to_owned(),
        lane: "fixture".to_owned(),
        cargo: TestCargoOptions::default(),
        runner: TestRunner::default(),
        features: Vec::new(),
    };
    let caller = [
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
    ]
    .map(str::to_owned);
    let outcome = lane
        .command(NextestAction::Run, &caller)
        .expect("canonical nextest lane command")
        .arg(&config)
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env("CARGO_TARGET_DIR", temp.path().join("build"))
        .current_dir(temp.path())
        .output()
        .expect("run the pinned nextest stress fixture");
    #[cfg(unix)]
    assert!(
        !cargo_home.join("stale-selected").exists(),
        "the canonical lane must bypass stale Cargo-home tools; nested exit {:?}, marker {:?}: {}",
        outcome.status.code(),
        fs::read_to_string(cargo_home.join("stale-selected")).ok(),
        String::from_utf8_lossy(&outcome.stderr)
    );
    let xml = fs::read_to_string(temp.path().join("target/nextest/default/junit.xml"))
        .expect("nextest retained both stress iterations");
    let cases = crate::junit::parse_junit(&xml).expect("parse actual nextest stress results");
    assert!(cases.iter().all(|case| {
        case.name == "an_earlier_iteration_fails" && case.suite == "nextest-stress-contract::stress"
    }));
    let mut iterations = cases
        .iter()
        .map(|case| (case.iteration, case.failed, case.flaky))
        .collect::<Vec<_>>();
    iterations.sort();
    assert_eq!(
        iterations,
        [(Some(0), true, false), (Some(1), false, false)],
        "the first iteration must fail and the last must pass without retries: {xml}"
    );
    assert_eq!(
        outcome.status.code(),
        Some(100),
        "an earlier stress failure must fail the command: {}",
        String::from_utf8_lossy(&outcome.stderr)
    );
}

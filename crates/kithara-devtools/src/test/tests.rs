use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
    process::Command,
};

use anyhow::Result;
use tempfile::TempDir;

use super::{
    command::{execute, lane_command, run_each, touched_command},
    request::TestRequest,
    selection::{lane_features, requested, select_lane, validate_config},
    *,
};
use crate::{
    common::project::{
        AuditClippyConfig, HealthConfig, KnownFlake, LintExcludeConfig, OrphansConfig, PerfConfig,
        ProjectConfig, ProjectIdentity, QualityConfig, StressConfig, TestCargoOptions,
        TestCargoRunner, TestCommandConfig, TestFlashConfig, TestLaneConfig, TestLoadConfig,
        TestNetBackendConfig, TestNextestRunner, TestNoBlockConfig, TestRunner, WorkspaceScan,
    },
    consts,
    touched::Touched,
    verdict::ChildFailure,
};

fn args_of(cmd: &Command) -> Vec<String> {
    cmd.get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect()
}

fn envs_of(cmd: &Command) -> Vec<(String, String)> {
    cmd.get_envs()
        .filter_map(|(key, value)| {
            value.map(|value| {
                (
                    key.to_string_lossy().into_owned(),
                    value.to_string_lossy().into_owned(),
                )
            })
        })
        .collect()
}

/// One lane's command under `caller`, with the lane's own defaults.
fn command_of(
    project: &ProjectConfig,
    lane: &str,
    action: NextestAction,
    caller: &[&str],
) -> Result<Command> {
    let caller = caller
        .iter()
        .map(|arg| (*arg).to_owned())
        .collect::<Vec<_>>();
    resolve(&project.test, &requested(&project.test, lane, None)?)?.command(action, &caller)
}

/// A lane that sets every typed key, so a render shows the order of each.
fn typed_lane() -> TestLaneConfig {
    TestLaneConfig {
        default_flash: Some(false),
        cargo: TestCargoOptions {
            profile: Some("test-release".to_owned()),
            packages: vec!["demo".to_owned()],
            tests: vec!["suite".to_owned()],
            lib: true,
            ..TestCargoOptions::default()
        },
        runner: TestRunner::Nextest(TestNextestRunner {
            filter: Some("test(fast)".to_owned()),
            test_threads: Some(1),
            ignore_default_filter: true,
        }),
        ..TestLaneConfig::default()
    }
}

/// The features a lane resolves to for `request`.
fn features_for(
    test: &TestCommandConfig,
    lane_name: &str,
    request: &TestRequest,
) -> Result<BTreeSet<String>> {
    let resolved = resolve(test, &requested(test, lane_name, Some(request))?)?;
    Ok(resolved.features.into_iter().collect())
}

/// An argument as the snapshot prints it: quoted when the shell would
/// otherwise split it.
fn quoted(arg: &str) -> String {
    if arg.is_empty() || arg.contains(char::is_whitespace) {
        format!("'{arg}'")
    } else {
        arg.to_owned()
    }
}

/// Every lane of `tests/fixtures/test-lanes.toml` renders the command
/// `test-lane-commands.txt` records, alone and under the two callers the
/// gates are: one that names a profile and one that names a filterset. The
/// fixture's lanes between them set every typed key, so a change to how a
/// key becomes an argument shows up as a reviewed diff.
#[test]
fn every_typed_lane_key_renders_the_recorded_command() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let test: TestCommandConfig = toml::from_str(
        &fs::read_to_string(fixtures.join("test-lanes.toml")).expect("the lane fixture exists"),
    )
    .expect("the lane fixture is a test configuration");
    let callers: [&[&str]; 3] = [
        &[],
        &["--profile", "ci", "--timings"],
        &["-E", "test(seek)"],
    ];
    let mut report = String::new();
    for name in test.lanes.keys() {
        for caller in callers {
            let mut args = vec![format!("--lane={name}")];
            args.extend(caller.iter().map(|arg| (*arg).to_owned()));
            let request = TestRequest::parse(&args).expect("parse request");
            let name = select_lane(&test, &request).expect("select lane");
            let command = lane_command(&test, name, &request).expect("lane command");
            report.push_str(&format!("# {name} [{}]\n", caller.join(" ")));
            report.push_str(&format!("  {}", command.get_program().to_string_lossy()));
            for arg in args_of(&command) {
                report.push(' ');
                report.push_str(&quoted(&arg));
            }
            report.push('\n');
            let mut envs = envs_of(&command);
            envs.sort();
            for (key, mut value) in envs {
                if key == "CARGO" && !test.lanes[name].env.contains_key("CARGO") {
                    assert_eq!(
                        value,
                        which::which(
                            cargo_metadata::MetadataCommand::new()
                                .cargo_command()
                                .get_program()
                        )
                        .expect("resolve Cargo")
                        .to_string_lossy()
                    );
                    value = "<cargo>".to_owned();
                }
                report.push_str(&format!("    {key}={value}\n"));
            }
        }
    }
    let snapshot = fixtures.join("test-lane-commands.txt");
    if std::env::var_os("KITHARA_UPDATE_SNAPSHOT").is_some() {
        fs::write(&snapshot, &report).expect("snapshot is writable");
    }
    let expected = fs::read_to_string(&snapshot).expect("the lane snapshot exists");
    assert_eq!(
        report, expected,
        "a test lane renders a different command than the snapshot records; \
         re-record with KITHARA_UPDATE_SNAPSHOT=1 only when the change is intended"
    );
}

pub(super) fn synthetic_project() -> ProjectConfig {
    let packages = |names: &[&str]| TestCargoOptions {
        packages: names.iter().map(|name| (*name).to_owned()).collect(),
        ..TestCargoOptions::default()
    };
    let workspace = TestCargoOptions {
        workspace: true,
        ..TestCargoOptions::default()
    };
    let mut lanes = BTreeMap::new();
    lanes.insert(
        "workspace".to_owned(),
        TestLaneConfig {
            cargo: workspace.clone(),
            ..TestLaneConfig::default()
        },
    );
    lanes.insert(
        "loom".to_owned(),
        TestLaneConfig {
            default_flash: Some(false),
            cargo: workspace,
            runner: TestRunner::Nextest(TestNextestRunner {
                filter: Some("test(loom_model_)".to_owned()),
                ..TestNextestRunner::default()
            }),
            default_features: vec!["demo/loom".to_owned()],
            ..TestLaneConfig::default()
        },
    );
    lanes.insert(
        "toolsmith".to_owned(),
        TestLaneConfig {
            cargo: packages(&["demo-tools"]),
            undeclared_toggles: vec![
                consts::FLASH_TOGGLE.to_owned(),
                consts::NO_BLOCK_TOGGLE.to_owned(),
                consts::LOAD_TOGGLE.to_owned(),
            ],
            ..TestLaneConfig::default()
        },
    );
    lanes.insert(
        "detector".to_owned(),
        TestLaneConfig {
            default_no_block: Some(true),
            default_load: Some(true),
            cargo: packages(&["demo-detector"]),
            ..TestLaneConfig::default()
        },
    );
    lanes.insert(
        "browser".to_owned(),
        TestLaneConfig {
            env: BTreeMap::from([("DEMO_BROWSER".to_owned(), "firefox".to_owned())]),
            default_flash: Some(false),
            cargo: TestCargoOptions {
                tests: vec!["web".to_owned()],
                ..packages(&["demo-web"])
            },
            runner: TestRunner::Cargo(TestCargoRunner {
                name_filters: vec!["selenium".to_owned()],
                no_capture: true,
                ..TestCargoRunner::default()
            }),
            ..TestLaneConfig::default()
        },
    );
    let mut net_backends = BTreeMap::new();
    net_backends.insert(
        "http".to_owned(),
        TestNetBackendConfig {
            features: Vec::new(),
        },
    );
    net_backends.insert(
        "native".to_owned(),
        TestNetBackendConfig {
            features: vec!["demo/native-net".to_owned()],
        },
    );
    ProjectConfig {
        ast_grep: crate::common::project::AstGrepConfig::default(),
        architecture: crate::common::project::ArchitectureConfig::default(),
        project: ProjectIdentity {
            name: "demo".to_owned(),
        },
        audit_clippy: AuditClippyConfig::default(),
        ci_report: crate::common::project::CiReportConfig::default(),
        clippy: crate::common::project::ClippyConfig::default(),
        health: HealthConfig::default(),
        test: TestCommandConfig {
            lanes,
            net_backends,
            shared_paths: Vec::new(),
            default_lane: "workspace".to_owned(),
            default_backend: "http".to_owned(),
            nextest_config: ".config/nextest.toml".to_owned(),
            known_flakes: Vec::new(),
            features: vec!["base-feature".to_owned()],
            flash: toml::from_str::<TestFlashConfig>("features = ['virtual-time']\ndefault = true")
                .expect("flash config"),
            no_block: toml::from_str::<TestNoBlockConfig>(
                "features = ['nb-detect']\ndefault = false",
            )
            .expect("no-block config"),
            load: toml::from_str::<TestLoadConfig>("features = ['cpu-load']\ndefault = false")
                .expect("load config"),
            loom_lane: "loom".to_owned(),
        },
        lint_exclude: LintExcludeConfig::default(),
        workspace_scan: WorkspaceScan::default(),
        orphans: OrphansConfig::default(),
        quality: QualityConfig::default(),
        perf: PerfConfig::default(),
        stress: StressConfig::default(),
        ext: toml::Table::default(),
        tools: crate::common::tools::ToolsConfig::default(),
    }
}

#[test]
fn lane_backend_default_does_not_override_an_explicit_request() {
    let mut project = synthetic_project();
    project.test.default_backend = "native".into();
    project
        .test
        .lanes
        .get_mut("workspace")
        .unwrap()
        .default_backend = Some("http".into());
    let default = TestRequest::parse(&["--flash=off".into()]).unwrap();
    let features = features_for(&project.test, "workspace", &default).unwrap();
    let explicit =
        TestRequest::parse(&["--flash=off".into(), "--net-backend=native".into()]).unwrap();
    let requested = features_for(&project.test, "workspace", &explicit).unwrap();
    assert_eq!(features, BTreeSet::from(["base-feature".into()]));
    assert_eq!(
        requested,
        BTreeSet::from(["base-feature".into(), "demo/native-net".into()])
    );
}

#[test]
fn lane_features_flash_and_backend() {
    let project = synthetic_project();
    let test = &project.test;
    let lane = &test.lanes[&test.default_lane];

    let feats = lane_features(
        test,
        lane,
        LaneToggles {
            flash: true,
            no_block: false,
            load: false,
        },
        "native",
    )
    .expect("features");
    assert!(feats.contains("base-feature"));
    assert!(feats.contains("virtual-time"));
    assert!(feats.contains("demo/native-net"));
    assert!(!feats.contains("nb-detect"));

    let feats = lane_features(
        test,
        lane,
        LaneToggles {
            flash: false,
            no_block: false,
            load: false,
        },
        "http",
    )
    .expect("features");
    assert_eq!(feats, BTreeSet::from(["base-feature".to_owned()]));
}

#[test]
fn features_default_request_omits_no_block() {
    let project = synthetic_project();
    let test = &project.test;
    let request = TestRequest::parse(&[]).expect("parse request");

    let feats = features_for(test, &test.default_lane, &request).expect("features");
    assert!(!feats.contains("nb-detect"));
}

#[test]
fn features_default_request_omits_load() {
    let project = synthetic_project();
    let request = TestRequest::parse(&[]).expect("parse request");

    let feats =
        features_for(&project.test, &project.test.default_lane, &request).expect("features");

    assert!(!feats.contains("cpu-load"));
}

#[test]
fn default_lane_command_resolves_cli_then_lane_then_project_backend() {
    let mut project = synthetic_project();
    project
        .test
        .lanes
        .get_mut("workspace")
        .expect("workspace")
        .default_backend = Some("native".to_owned());

    let lane_backend = lane_command(
        &project.test,
        &project.test.default_lane,
        &TestRequest::parse(&[]).expect("parse request"),
    )
    .expect("default lane command");
    assert!(
        args_of(&lane_backend)
            .iter()
            .any(|arg| arg.contains("demo/native-net"))
    );

    let cli_backend = lane_command(
        &project.test,
        &project.test.default_lane,
        &TestRequest::parse(&["--net-backend=http".to_owned()]).expect("parse request"),
    )
    .expect("default lane command");
    assert!(
        !args_of(&cli_backend)
            .iter()
            .any(|arg| arg.contains("demo/native-net"))
    );
}

#[test]
fn lane_backend_must_be_configured() {
    let mut project = synthetic_project();
    project
        .test
        .lanes
        .get_mut("browser")
        .expect("browser")
        .default_backend = Some("missing".to_owned());

    let error = validate_config(&project.test).expect_err("unknown lane backend fails");

    assert!(
        error
            .to_string()
            .contains("test.lanes.browser.default_backend")
    );
}

#[test]
fn no_block_on_adds_features_and_composes_with_flash_and_backend() {
    let project = synthetic_project();
    let test = &project.test;
    let request = TestRequest::parse(&[
        "--flash=on".to_owned(),
        "--no-block=on".to_owned(),
        "--net-backend=native".to_owned(),
    ])
    .expect("parse request");

    let feats = features_for(test, &test.default_lane, &request).expect("features");
    assert!(feats.contains("base-feature"));
    assert!(feats.contains("virtual-time"));
    assert!(feats.contains("demo/native-net"));
    assert!(feats.contains("nb-detect"));
}

#[test]
fn no_block_off_keeps_no_block_features_out() {
    let project = synthetic_project();
    let test = &project.test;
    let request = TestRequest::parse(&[
        "--flash=on".to_owned(),
        "--no-block=off".to_owned(),
        "--net-backend=native".to_owned(),
    ])
    .expect("parse request");

    let feats = features_for(test, &test.default_lane, &request).expect("features");
    assert!(!feats.contains("nb-detect"));
    assert!(feats.contains("virtual-time"));
}

#[test]
fn load_on_adds_features_and_composes_with_other_toggles_and_backend() {
    let project = synthetic_project();
    let request = TestRequest::parse(&[
        "--flash=on".to_owned(),
        "--no-block=on".to_owned(),
        "--load=on".to_owned(),
        "--net-backend=native".to_owned(),
    ])
    .expect("parse request");

    let feats =
        features_for(&project.test, &project.test.default_lane, &request).expect("features");

    assert_eq!(
        feats,
        BTreeSet::from([
            "base-feature".to_owned(),
            "cpu-load".to_owned(),
            "demo/native-net".to_owned(),
            "nb-detect".to_owned(),
            "virtual-time".to_owned(),
        ])
    );
}

#[test]
fn load_off_keeps_load_features_out() {
    let project = synthetic_project();
    let request = TestRequest::parse(&["--load=off".to_owned()]).expect("parse request");

    let feats =
        features_for(&project.test, &project.test.default_lane, &request).expect("features");

    assert!(!feats.contains("cpu-load"));
    assert!(feats.contains("virtual-time"));
}

#[test]
fn a_lane_that_asks_for_the_detector_gets_it_without_a_flag() {
    let project = synthetic_project();
    let test = &project.test;
    let request = TestRequest::parse(&[]).expect("parse request");

    let feats = features_for(test, "detector", &request).expect("features");

    assert!(feats.contains("nb-detect"));
}

#[test]
fn a_lane_that_asks_for_load_gets_it_without_a_flag() {
    let project = synthetic_project();
    let request = TestRequest::parse(&[]).expect("parse request");

    let feats = features_for(&project.test, "detector", &request).expect("features");

    assert!(feats.contains("cpu-load"));
}

#[test]
fn an_explicit_off_overrides_the_lane_detector_default() {
    let project = synthetic_project();
    let test = &project.test;
    let request = TestRequest::parse(&["--no-block=off".to_owned()]).expect("parse request");

    let feats = features_for(test, "detector", &request).expect("features");

    assert!(!feats.contains("nb-detect"));
}

#[test]
fn an_explicit_off_overrides_the_lane_load_default() {
    let project = synthetic_project();
    let request = TestRequest::parse(&["--load=off".to_owned()]).expect("parse request");

    let feats = features_for(&project.test, "detector", &request).expect("features");

    assert!(!feats.contains("cpu-load"));
    assert!(feats.contains("nb-detect"));
}

#[test]
fn a_lane_without_the_detector_stays_without_it_when_the_gate_asks_for_it() {
    let project = synthetic_project();
    let test = &project.test;
    let request = TestRequest::parse(&["--no-block=on".to_owned()]).expect("parse request");

    let feats = features_for(test, "toolsmith", &request).expect("features");

    assert!(
        !feats.contains("nb-detect"),
        "a lane whose packages do not declare the feature cannot be given it",
    );
}

#[test]
fn a_lane_without_load_stays_without_it_when_asked_for_it() {
    let mut project = synthetic_project();
    project.test.load.enabled = true;
    project
        .test
        .lanes
        .get_mut("toolsmith")
        .expect("toolsmith lane")
        .default_load = Some(true);

    for args in [vec![], vec!["--load=on".to_owned()]] {
        let request = TestRequest::parse(&args).expect("parse request");
        let choice = requested(&project.test, "toolsmith", Some(&request)).expect("lane");
        let feats = features_for(&project.test, "toolsmith", &request).expect("features");

        assert!(!choice.toggles.load);
        assert!(!feats.contains("cpu-load"));
    }
}

#[test]
fn load_resolves_cli_then_lane_then_project_default() {
    let mut project = synthetic_project();
    project.test.load.enabled = true;
    let default = TestRequest::parse(&[]).expect("parse request");

    assert!(
        features_for(&project.test, "workspace", &default)
            .expect("features")
            .contains("cpu-load")
    );

    project
        .test
        .lanes
        .get_mut("workspace")
        .expect("workspace lane")
        .default_load = Some(false);

    assert!(
        !features_for(&project.test, "workspace", &default)
            .expect("features")
            .contains("cpu-load")
    );

    let explicit = TestRequest::parse(&["--load=on".to_owned()]).expect("parse request");
    assert!(
        features_for(&project.test, "workspace", &explicit)
            .expect("features")
            .contains("cpu-load")
    );
}

#[test]
fn a_lane_without_the_virtual_clock_stays_without_it_when_asked_for_it() {
    let project = synthetic_project();
    let test = &project.test;
    let request = TestRequest::parse(&["--flash=on".to_owned()]).expect("parse request");

    let feats = features_for(test, "toolsmith", &request).expect("features");

    assert!(
        !feats.contains("virtual-time"),
        "a lane whose packages do not declare the feature cannot be given it",
    );
}

#[test]
fn no_block_bogus_mode_is_a_typed_error() {
    let error = TestRequest::parse(&["--no-block".to_owned(), "bogus".to_owned()])
        .expect_err("parse invalid no-block");

    assert!(error.to_string().contains("no-block"));
}

#[test]
fn no_block_space_form_parses_to_on() {
    let project = synthetic_project();
    let test = &project.test;
    let request =
        TestRequest::parse(&["--no-block".to_owned(), "on".to_owned()]).expect("parse request");

    let feats = features_for(test, &test.default_lane, &request).expect("features");
    assert!(feats.contains("nb-detect"));
}

#[test]
fn load_equals_and_space_forms_parse_all_toggle_values() {
    for (value, expected) in [
        ("on", true),
        ("true", true),
        ("off", false),
        ("false", false),
    ] {
        for args in [
            vec![format!("--load={value}")],
            vec!["--load".to_owned(), value.to_owned()],
        ] {
            let request = TestRequest::parse(&args).expect("parse request");

            assert_eq!(request.load, Some(expected));
            assert!(request.passthrough.is_empty());
        }
    }
}

#[test]
fn load_invalid_or_missing_value_is_a_typed_error() {
    for args in [
        vec!["--load".to_owned()],
        vec!["--load=".to_owned()],
        vec!["--load=bogus".to_owned()],
        vec!["--load".to_owned(), "bogus".to_owned()],
    ] {
        let error = TestRequest::parse(&args).expect_err("parse invalid load");

        assert!(error.to_string().contains("load"));
    }
}

#[test]
fn the_default_lane_renders_its_selection_features_and_the_callers_profile() {
    let project = synthetic_project();

    let command = command_of(
        &project,
        "workspace",
        NextestAction::Run,
        &["--profile", "perf"],
    )
    .expect("command");

    assert_eq!(command.get_program().to_string_lossy(), "cargo-nextest");
    assert_eq!(
        args_of(&command),
        [
            "nextest",
            "run",
            "--workspace",
            "--features",
            "base-feature,virtual-time",
            "--profile",
            "perf",
        ]
    );
}

#[test]
fn package_scope_replaces_workspace_selection() {
    let mut project = synthetic_project();
    project
        .test
        .lanes
        .get_mut("workspace")
        .expect("workspace lane")
        .cargo
        .exclude
        .push("excluded-package".to_owned());

    let args = args_of(
        &command_of(
            &project,
            "workspace",
            NextestAction::Run,
            &["-p", "one-package"],
        )
        .expect("command"),
    );

    assert!(!args.contains(&"--workspace".to_owned()));
    assert!(!args.contains(&"--exclude".to_owned()));
    assert!(args.windows(2).any(|args| args == ["-p", "one-package"]));
}

#[test]
fn the_inventory_lists_the_same_selection_the_run_builds() {
    let project = synthetic_project();
    let resolved = resolve(
        &project.test,
        &LaneChoice {
            backend: "http",
            lane: "workspace",
            toggles: LaneToggles {
                flash: true,
                no_block: true,
                load: true,
            },
        },
    )
    .expect("resolve");

    let command = resolved
        .command(
            NextestAction::List,
            &["--message-format".to_owned(), "json".to_owned()],
        )
        .expect("list command");

    assert_eq!(
        args_of(&command),
        [
            "nextest",
            "list",
            "--workspace",
            "--features",
            "base-feature,cpu-load,nb-detect,virtual-time",
            "--message-format",
            "json",
        ]
    );
}

#[test]
fn loom_flag_selects_model_lane_and_composes_with_flash() {
    let project = synthetic_project();
    let request = TestRequest::parse(&["--loom=on".to_owned(), "--flash=on".to_owned()])
        .expect("parse request");

    let name = select_lane(&project.test, &request).expect("select loom lane");
    assert_eq!(name, "loom");
    let features = features_for(&project.test, name, &request).expect("loom features");
    assert_eq!(
        features,
        BTreeSet::from([
            "base-feature".to_owned(),
            "demo/loom".to_owned(),
            "virtual-time".to_owned(),
        ])
    );
}

#[test]
fn loom_flag_with_no_block_on_composes_features() {
    let project = synthetic_project();
    let request = TestRequest::parse(&["--loom=on".to_owned(), "--no-block=on".to_owned()])
        .expect("parse request");

    let name = select_lane(&project.test, &request).expect("select loom lane");
    assert_eq!(name, "loom");
    let features = features_for(&project.test, name, &request).expect("loom features");
    assert_eq!(
        features,
        BTreeSet::from([
            "base-feature".to_owned(),
            "demo/loom".to_owned(),
            "nb-detect".to_owned(),
        ])
    );
}

#[test]
fn loom_flag_with_load_on_composes_features() {
    let project = synthetic_project();
    let request = TestRequest::parse(&["--loom=on".to_owned(), "--load=on".to_owned()])
        .expect("parse request");

    let name = select_lane(&project.test, &request).expect("select loom lane");
    let features = features_for(&project.test, name, &request).expect("loom features");

    assert_eq!(name, "loom");
    assert_eq!(
        features,
        BTreeSet::from([
            "base-feature".to_owned(),
            "cpu-load".to_owned(),
            "demo/loom".to_owned(),
        ])
    );
}

#[test]
fn loom_flag_rejects_an_explicit_non_model_lane() {
    let project = synthetic_project();
    let request = TestRequest::parse(&["--loom=on".to_owned(), "--lane=workspace".to_owned()])
        .expect("parse request");

    let error = select_lane(&project.test, &request).expect_err("lane conflict");
    assert!(
        error
            .to_string()
            .contains("conflicts with --lane=workspace")
    );
}

#[test]
fn a_lane_that_names_an_environment_runs_with_it() {
    let project = synthetic_project();
    let request = TestRequest::parse(&["--lane=browser".to_owned()]).expect("parse request");
    let name = select_lane(&project.test, &request).expect("select browser lane");

    let cmd = lane_command(&project.test, name, &request).expect("browser lane command");

    assert_eq!(
        envs_of(&cmd),
        vec![("DEMO_BROWSER".to_owned(), "firefox".to_owned())]
    );
}

/// A lane says which tests it runs with a filter, never with a runner profile
/// of its own: the caller's profile is how a gate gives every lane one retry
/// policy and one report, and a lane that swapped it out kept neither.
#[test]
fn a_lane_cannot_name_a_runner_profile() {
    let error = toml::from_str::<TestCommandConfig>(
        "[lanes.tools]\ncargo.packages = [\"demo-tools\"]\nrunner.nextest.profile = \"support\"\n",
    )
    .expect_err("a lane that names a runner profile is refused");

    assert!(
        error.to_string().contains("unknown field `profile`"),
        "{error}"
    );
}

#[test]
fn every_lane_takes_the_callers_profile() {
    let project = synthetic_project();
    let request = TestRequest::parse(&["--lane=detector", "--profile", "ci"].map(str::to_owned))
        .expect("parse request");
    let name = select_lane(&project.test, &request).expect("select detector lane");

    let cmd = lane_command(&project.test, name, &request).expect("detector lane command");

    assert!(
        args_of(&cmd)
            .windows(2)
            .any(|pair| pair == ["--profile", "ci"])
    );
}

#[test]
fn a_nextest_lane_supplies_cargo_unless_the_lane_names_it() {
    let mut project = synthetic_project();
    let request = TestRequest::parse(&[]).expect("parse request");
    let name = select_lane(&project.test, &request).expect("select default lane");

    let cmd = lane_command(&project.test, name, &request).expect("default lane command");

    assert_eq!(
        envs_of(&cmd),
        vec![(
            "CARGO".to_owned(),
            which::which(
                cargo_metadata::MetadataCommand::new()
                    .cargo_command()
                    .get_program()
            )
            .expect("resolve Cargo")
            .to_string_lossy()
            .into_owned()
        )]
    );

    project
        .test
        .lanes
        .get_mut("workspace")
        .expect("default lane")
        .env
        .insert("CARGO".to_owned(), "lane-cargo".to_owned());
    let command = lane_command(&project.test, "workspace", &request).expect("lane override");
    assert_eq!(
        envs_of(&command),
        vec![("CARGO".to_owned(), "lane-cargo".to_owned())]
    );
}

#[test]
fn a_nextest_lane_resolves_relative_cargo_before_its_path_changes() {
    const CHILD: &str = "KITHARA_RELATIVE_CARGO_PROBE";
    if std::env::var_os(CHILD).is_none() {
        let output = Command::new(std::env::current_exe().expect("test executable"))
            .args([
                "--exact",
                "test::tests::a_nextest_lane_resolves_relative_cargo_before_its_path_changes",
                "--nocapture",
            ])
            .env("CARGO", "cargo")
            .env(CHILD, "1")
            .output()
            .expect("run the relative Cargo contract");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains("1 passed"),
            "relative Cargo contract failed: {stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    let temp = TempDir::new().expect("lane PATH directory");
    let mut project = synthetic_project();
    project
        .test
        .lanes
        .get_mut("workspace")
        .expect("default lane")
        .env
        .insert("PATH".to_owned(), temp.path().display().to_string());
    let request = TestRequest::parse(&[]).expect("parse request");
    let command = lane_command(&project.test, "workspace", &request).expect("lane command");
    let envs = envs_of(&command);
    let (_, cargo) = envs
        .iter()
        .find(|(key, _)| key == "CARGO")
        .expect("the lane supplies Cargo");
    let cargo = Path::new(cargo);
    assert!(cargo.is_absolute(), "Cargo remains relative: {cargo:?}");
    assert_eq!(
        cargo.file_name().expect("Cargo filename"),
        format!("cargo{}", std::env::consts::EXE_SUFFIX).as_str()
    );
    assert!(!cargo.starts_with(temp.path()), "Cargo uses the lane PATH");
}

/// A lane is clean only once its report agrees. The verdict lives past
/// the exit code because nextest exits zero on a retried pass, so a lane
/// read by status alone reported a defect that reproduced as a clean run.
///
/// The lane runs a program that exits without building anything while its
/// command line still names the runner profile it would have run under.
#[test]
fn a_green_status_is_not_the_whole_verdict_of_a_retrying_lane() {
    let temp = TempDir::new().expect("temp root");
    fs::create_dir_all(temp.path().join(".config")).expect("create .config");
    fs::write(
        temp.path().join(".config").join("nextest.toml"),
        "[profile.ci]\nretries = 1\n",
    )
    .expect("write nextest config");
    let mut project = synthetic_project();
    project.test.nextest_config = ".config/nextest.toml".to_owned();
    let mut command = Command::new("cargo");
    command.args(["--version", "nextest", "run", "--profile", "ci"]);

    let error = execute(&project.test, temp.path(), "retrying", &mut command)
        .expect_err("a lane that ran a retrying profile is judged by its report")
        .to_string();

    assert!(error.contains("declares no JUnit report"), "{error}");
}

#[cfg(unix)]
fn failing(args: &[&str]) -> Command {
    let mut command = Command::new("false");
    command.args(args);
    command
}

/// A red lane blames only what this run recorded: the report a previous
/// run left in a shared build directory must not name tests this one
/// never reached.
#[cfg(unix)]
#[test]
fn a_lane_that_fails_before_its_tests_does_not_inherit_the_previous_report() {
    let temp = TempDir::new().expect("temp root");
    fs::create_dir_all(temp.path().join(".config")).expect("create .config");
    fs::write(
        temp.path().join(".config").join("nextest.toml"),
        consts::RETRYING_PROFILE,
    )
    .expect("write nextest config");
    let store = temp.path().join("target").join("nextest").join("ci");
    fs::create_dir_all(&store).expect("create store");
    fs::write(store.join("junit.xml"), consts::FAILED_AND_RETRIED).expect("leave a report");
    let mut project = synthetic_project();
    project.test.nextest_config = ".config/nextest.toml".to_owned();

    let error = execute(
        &project.test,
        temp.path(),
        "broken",
        &mut failing(&["nextest", "run", "--profile", "ci"]),
    )
    .expect_err("the lane exits non-zero")
    .to_string();

    assert!(error.contains("left no test report"), "{error}");
    assert!(!error.contains("stalled_target"), "{error}");
}

/// Removing the previous report only keeps the verdict honest: a report
/// the lane cannot remove must not stop the lane from running.
#[cfg(unix)]
#[test]
fn a_lane_whose_previous_report_cannot_be_removed_still_runs() {
    let temp = TempDir::new().expect("temp root");
    fs::create_dir_all(temp.path().join(".config")).expect("create .config");
    fs::write(
        temp.path().join(".config").join("nextest.toml"),
        consts::RETRYING_PROFILE,
    )
    .expect("write nextest config");
    let report = temp
        .path()
        .join("target")
        .join("nextest")
        .join("ci")
        .join("junit.xml");
    fs::create_dir_all(&report).expect("leave a report path no file removal clears");
    let mut project = synthetic_project();
    project.test.nextest_config = ".config/nextest.toml".to_owned();

    let error = execute(
        &project.test,
        temp.path(),
        "broken",
        &mut failing(&["nextest", "run", "--profile", "ci"]),
    )
    .expect_err("the lane exits non-zero");

    assert_eq!(
        error
            .downcast_ref::<ChildFailure>()
            .map(ChildFailure::exit_code),
        Some(1),
        "{error:#}"
    );
}

/// The gate's last line names every red lane with its own reason, not
/// only that some failed.
#[cfg(unix)]
#[test]
fn every_red_touched_lane_keeps_its_own_error() {
    let project = synthetic_project();

    let error = run_each(&["first".to_owned(), "second".to_owned()], |lane| {
        execute(&project.test, Path::new("."), lane, &mut failing(&[]))
    })
    .expect_err("both lanes fail");

    assert_eq!(
        error
            .downcast_ref::<ChildFailure>()
            .map(ChildFailure::exit_code),
        Some(1)
    );
    let error = error.to_string();
    assert!(
        error.contains("test lane `first` failed (exit code 1)"),
        "{error}"
    );
    assert!(
        error.contains("test lane `second` failed (exit code 1)"),
        "{error}"
    );
}

#[test]
fn a_tolerated_flake_without_an_owner_is_not_configuration() {
    let mut project = synthetic_project();
    project.test.known_flakes = vec![KnownFlake {
        test: "kithara_queue::delayed_target".to_owned(),
        issue: String::new(),
    }];

    let error = validate_config(&project.test)
        .expect_err("an entry nobody owns is a flake nobody removes")
        .to_string();

    assert!(error.contains("names no issue"), "{error}");
}

#[test]
fn a_resolved_lane_carries_its_environment_to_the_runner() {
    let project = synthetic_project();

    let resolved = resolve(
        &project.test,
        &requested(&project.test, "browser", None).expect("lane"),
    )
    .expect("resolve");

    assert_eq!(resolved.env["DEMO_BROWSER"], "firefox");
    let command = resolved.command(NextestAction::Run, &[]).expect("command");
    assert_eq!(
        envs_of(&command),
        vec![("DEMO_BROWSER".to_owned(), "firefox".to_owned())]
    );
}

#[test]
fn a_platform_adapter_can_build_a_named_lane_inventory() {
    let project = synthetic_project();
    let command = nextest_command_for_lane(
        &project,
        "loom",
        &["-p".to_owned(), "demo-platform-tests".to_owned()],
        NextestAction::List,
    )
    .expect("named lane inventory");

    assert_eq!(
        args_of(&command),
        [
            "nextest",
            "list",
            "--features",
            "base-feature,demo/loom",
            "-E",
            "test(loom_model_)",
            "-p",
            "demo-platform-tests",
        ]
    );
}

#[test]
fn a_nextest_lane_renders_its_typed_options_in_one_order() {
    let mut project = synthetic_project();
    project.test.lanes.insert("typed".to_owned(), typed_lane());

    let command =
        command_of(&project, "typed", NextestAction::Run, &["--timings"]).expect("command");

    assert_eq!(
        args_of(&command),
        [
            "nextest",
            "run",
            "-p",
            "demo",
            "--cargo-profile",
            "test-release",
            "--lib",
            "--test",
            "suite",
            "--features",
            "base-feature",
            "--test-threads",
            "1",
            "--ignore-default-filter",
            "-E",
            "test(fast)",
            "--timings",
        ]
    );
}

/// `nextest list` refuses `--test-threads`; a thread count only shapes a
/// run.
#[test]
fn listing_a_lane_leaves_its_thread_count_to_the_run() {
    let mut project = synthetic_project();
    project.test.lanes.insert("typed".to_owned(), typed_lane());

    let args = args_of(&command_of(&project, "typed", NextestAction::List, &[]).expect("list"));

    assert_eq!(
        args.get(..2),
        Some(["nextest", "list"].map(str::to_owned).as_slice())
    );
    assert!(!args.contains(&"--test-threads".to_owned()), "{args:?}");
}

#[test]
fn a_cargo_lane_hands_its_name_filters_to_the_test_binary() {
    let project = synthetic_project();

    let command = command_of(
        &project,
        "browser",
        NextestAction::Run,
        &["--timings", "--", "--ignored"],
    )
    .expect("command");

    assert_eq!(
        args_of(&command),
        [
            "test",
            "-p",
            "demo-web",
            "--test",
            "web",
            "--features",
            "base-feature",
            "--timings",
            "--",
            "selenium",
            "--nocapture",
            "--ignored",
        ]
    );
}

#[test]
fn a_doc_lane_runs_the_doctests_of_its_selection() {
    let mut project = synthetic_project();
    project.test.lanes.insert(
        "doc".to_owned(),
        TestLaneConfig {
            default_flash: Some(false),
            cargo: TestCargoOptions {
                profile: Some("test-release".to_owned()),
                exclude: vec!["demo-fuzz".to_owned()],
                workspace: true,
                ..TestCargoOptions::default()
            },
            runner: TestRunner::Cargo(TestCargoRunner {
                doc: true,
                ..TestCargoRunner::default()
            }),
            ..TestLaneConfig::default()
        },
    );

    let command =
        command_of(&project, "doc", NextestAction::Run, &["--profile", "ci"]).expect("command");

    assert_eq!(
        args_of(&command),
        [
            "test",
            "--doc",
            "--workspace",
            "--exclude",
            "demo-fuzz",
            "--profile",
            "test-release",
            "--features",
            "base-feature",
        ]
    );
}

#[test]
fn a_cargo_lane_has_no_inventory_to_list() {
    let project = synthetic_project();

    let error = command_of(&project, "browser", NextestAction::List, &[])
        .expect_err("cargo test lists nothing")
        .to_string();

    assert!(error.contains("has no inventory to list"), "{error}");
}

#[test]
fn a_lane_selects_exactly_one_of_the_workspace_and_its_packages() {
    for cargo in [
        TestCargoOptions::default(),
        TestCargoOptions {
            packages: vec!["demo".to_owned()],
            workspace: true,
            ..TestCargoOptions::default()
        },
    ] {
        let mut project = synthetic_project();
        project.test.lanes.insert(
            "odd".to_owned(),
            TestLaneConfig {
                cargo,
                ..TestLaneConfig::default()
            },
        );

        let error = validate_config(&project.test)
            .expect_err("a lane selects one way")
            .to_string();

        assert!(
            error.contains("test.lanes.odd.cargo needs exactly one"),
            "{error}"
        );
    }
}

#[test]
fn an_exclusion_needs_a_workspace_lane() {
    let mut project = synthetic_project();
    project.test.lanes.insert(
        "odd".to_owned(),
        TestLaneConfig {
            cargo: TestCargoOptions {
                exclude: vec!["demo-fuzz".to_owned()],
                packages: vec!["demo".to_owned()],
                ..TestCargoOptions::default()
            },
            ..TestLaneConfig::default()
        },
    );

    let error = validate_config(&project.test)
        .expect_err("exclude narrows only a workspace")
        .to_string();

    assert!(
        error.contains("test.lanes.odd.cargo.exclude needs"),
        "{error}"
    );
}

/// A caller's package selection replaces any lane's workspace
/// selection, so `-p` narrows the doc and loom lanes as it narrows the
/// default one.
#[test]
fn a_caller_package_narrows_every_workspace_lane() {
    let project = synthetic_project();

    let args = args_of(
        &command_of(
            &project,
            "loom",
            NextestAction::Run,
            &["--package=demo-core"],
        )
        .expect("command"),
    );

    assert!(!args.contains(&"--workspace".to_owned()), "{args:?}");
    assert!(args.contains(&"--package=demo-core".to_owned()), "{args:?}");
}

/// The one difference between the two runners' cargo arguments is the
/// spelling of the Cargo profile.
#[test]
fn the_runners_translate_only_the_profile_flag() {
    let mut project = synthetic_project();
    project.test.lanes.insert("typed".to_owned(), typed_lane());
    let resolved = resolve(
        &project.test,
        &requested(&project.test, "typed", None).expect("lane"),
    )
    .expect("resolve");

    let for_nextest = resolved.cargo_args("--cargo-profile", false);
    let for_cargo = resolved.cargo_args("--profile", false);

    assert_eq!(
        for_nextest
            .iter()
            .map(|arg| if arg == "--cargo-profile" {
                "--profile"
            } else {
                arg.as_str()
            })
            .collect::<Vec<_>>(),
        for_cargo
    );
}

/// nextest unions repeated `-E`, so a caller filterset would widen a
/// filtered lane; the lane's filter and the caller's are intersected into
/// one expression instead, whatever spelling the caller used.
#[test]
fn a_caller_filterset_narrows_a_filtered_lane() {
    let project = synthetic_project();

    let args = args_of(
        &command_of(
            &project,
            "loom",
            NextestAction::Run,
            &[
                "-E",
                "test(seek)",
                "--filterset",
                "test(a)",
                "--filter-expr=test(b)",
                "-Etest(c)",
                "--timings",
            ],
        )
        .expect("command"),
    );

    assert_eq!(
        args.iter().filter(|arg| *arg == "-E").count(),
        1,
        "{args:?}"
    );
    assert!(
        args.windows(2).any(|pair| pair
            == [
                "-E",
                "(test(loom_model_)) & ((test(seek)) | (test(a)) | (test(b)) | (test(c)))"
            ]),
        "{args:?}"
    );
    assert!(
        !args.iter().any(|arg| arg.starts_with("--filter")),
        "{args:?}"
    );
    assert_eq!(args.last().map(String::as_str), Some("--timings"));
}

/// A filterset flag the caller leaves without a value is a mistyped request,
/// not a request for the lane's whole selection.
#[test]
fn a_caller_filterset_flag_without_a_value_is_refused() {
    let project = synthetic_project();

    for flag in ["-E", "--filterset", "--filter-expr"] {
        let error = command_of(&project, "loom", NextestAction::Run, &["--timings", flag])
            .expect_err("a filterset flag without a value");
        assert!(
            format!("{error:#}").contains(&format!("`{flag}` needs a filterset")),
            "{error:#}"
        );
    }
}

#[test]
fn a_caller_filterset_passes_through_an_unfiltered_lane() {
    let project = synthetic_project();

    let args = args_of(
        &command_of(
            &project,
            "workspace",
            NextestAction::Run,
            &["-E", "test(seek)"],
        )
        .expect("command"),
    );

    assert!(
        args.ends_with(&["-E".to_owned(), "test(seek)".to_owned()]),
        "{args:?}"
    );
}

/// A touched run narrowed to packages builds what the whole lane builds: its
/// command is the whole lane's with one filterset more, and a caller's
/// filterset narrows that one further.
#[test]
fn a_narrowed_touched_run_builds_what_the_whole_lane_builds() {
    let project = synthetic_project();
    let request = TestRequest::parse(&[
        "--profile".to_owned(),
        "ci".to_owned(),
        "-E".to_owned(),
        "test(seek)".to_owned(),
    ])
    .expect("parse request");
    let narrowed = Touched::Narrowed {
        lane: "workspace".to_owned(),
        packages: BTreeSet::from(["demo".to_owned(), "demo-web".to_owned()]),
    };
    let without_filtersets = |args: Vec<String>| {
        let mut kept = Vec::new();
        let mut iter = args.into_iter();
        while let Some(arg) = iter.next() {
            if arg == "-E" {
                iter.next();
            } else {
                kept.push(arg);
            }
        }
        kept
    };

    let whole = args_of(
        &touched_command(
            &project.test,
            &Touched::Whole("workspace".to_owned()),
            &request,
        )
        .expect("whole command"),
    );
    let narrowed =
        args_of(&touched_command(&project.test, &narrowed, &request).expect("narrowed command"));

    assert_eq!(
        without_filtersets(narrowed.clone()),
        without_filtersets(whole)
    );
    assert!(
        narrowed
            .windows(2)
            .any(|pair| pair == ["-E", "(package(demo) | package(demo-web)) & ((test(seek)))"]),
        "{narrowed:?}"
    );
}

/// A `cargo test` lane has no filterset to narrow it by package.
#[test]
fn a_cargo_test_lane_refuses_a_narrowed_run() {
    let project = synthetic_project();
    let request = TestRequest::parse(&[]).expect("parse request");
    let narrowed = Touched::Narrowed {
        lane: "browser".to_owned(),
        packages: BTreeSet::from(["demo-web".to_owned()]),
    };

    let error = touched_command(&project.test, &narrowed, &request)
        .expect_err("a cargo test lane narrowed by package");

    assert!(
        format!("{error:#}").contains("a filterset cannot narrow"),
        "{error:#}"
    );
}

fn request_of(args: &[&str]) -> Result<TestRequest> {
    TestRequest::parse(&args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>())
}

fn filtersets(args: &[String]) -> Vec<&str> {
    args.windows(2)
        .filter(|pair| pair[0] == "-E")
        .map(|pair| pair[1].as_str())
        .collect()
}

/// A narrowed request only takes tests away: its filterset is intersected
/// with the lane's own and with the caller's, whose filtersets nextest would
/// otherwise union with it.
#[test]
fn a_narrowed_request_runs_only_what_every_filterset_selects() {
    let project = synthetic_project();
    let request = request_of(&["--narrow=test(seek)", "-E", "test(a)", "-E", "test(b)"])
        .expect("parse request");

    let filtered = args_of(&lane_command(&project.test, "loom", &request).expect("command"));
    let unfiltered = args_of(&lane_command(&project.test, "workspace", &request).expect("command"));

    assert_eq!(
        filtersets(&filtered),
        ["((test(loom_model_)) & ((test(seek)))) & ((test(a)) | (test(b)))"],
        "{filtered:?}"
    );
    assert_eq!(
        filtersets(&unfiltered),
        ["(test(seek)) & ((test(a)) | (test(b)))"],
        "{unfiltered:?}"
    );
    assert!(
        !filtered
            .iter()
            .chain(&unfiltered)
            .any(|arg| arg.contains("--narrow")),
        "the harness consumes its own flag"
    );
}

/// A touched run narrowed to packages is narrowed by the request as well.
#[test]
fn a_narrowed_request_narrows_a_touched_run() {
    let project = synthetic_project();
    let request = request_of(&["--narrow", "test(seek)"]).expect("parse request");
    let touched = Touched::Narrowed {
        lane: "workspace".to_owned(),
        packages: BTreeSet::from(["demo".to_owned()]),
    };

    let args = args_of(&touched_command(&project.test, &touched, &request).expect("command"));

    assert_eq!(
        filtersets(&args),
        ["(test(seek)) & ((package(demo)))"],
        "{args:?}"
    );
}

/// A `cargo test` lane has no filterset, so a narrowed request cannot be
/// honoured there and is refused rather than run whole.
#[test]
fn a_cargo_test_lane_refuses_a_narrowed_request() {
    let project = synthetic_project();
    let request = request_of(&["--narrow=test(seek)"]).expect("parse request");

    let error = lane_command(&project.test, "browser", &request)
        .expect_err("a cargo test lane asked to narrow");

    assert!(
        format!("{error:#}").contains("a filterset cannot narrow"),
        "{error:#}"
    );
}

/// A narrowing flag without a filterset is a mistyped request, not a request
/// for the whole lane.
#[test]
fn a_narrowing_flag_without_a_filterset_is_refused() {
    for args in [&["--narrow"][..], &["--narrow="], &["--narrow", " "]] {
        let error = request_of(args).expect_err("a narrowing flag without a filterset");
        assert!(
            format!("{error:#}").contains("--narrow needs a filterset"),
            "{args:?}: {error:#}"
        );
    }
}

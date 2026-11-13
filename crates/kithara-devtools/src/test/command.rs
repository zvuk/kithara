use std::{fs, path::Path, process::Command};

use anyhow::{Context, Result};
use clap::Args;

use super::{
    draft::{DraftScope, PR_TESTS_ENV},
    inventory::admit_expected,
    request::TestRequest,
    resolve::{ResolvedLane, resolve},
    selection::{requested, select_lane, validate_config},
};
use crate::{
    Ctx,
    common::project::{ProjectConfig, TestCommandConfig, TestRunner},
    retried::Evidence,
};

#[derive(Debug, Args)]
#[command(trailing_var_arg = true)]
pub struct TestArgs {
    /// Arguments for the configured test command. Recipe-level flags accepted anywhere:
    /// `--lane=<configured-name>`, `--flash=true|false|on|off`, `--no-flash`,
    /// `--loom=true|false|on|off`, `--no-loom`, `--no-block=true|false|on|off`,
    /// `--load=true|false|on|off`, `--net-backend=<configured-name>`, `--narrow=<filterset>`,
    /// `--draft` (the nextest lane's share of the required packages and the pull request's
    /// `KITHARA_PR_TESTS` block; admitted Cargo suites run whole),
    /// `--selection-output=<path>` (JSON lane names), and `--expected-tests=<path>`
    /// (UTF-8 TSV: binary ID, tab, full test name per line).
    #[arg(value_name = "ARGS", allow_hyphen_values = true)]
    pub(crate) args: Vec<String>,
}

pub(crate) fn run(args: &TestArgs, ctx: &Ctx) -> Result<()> {
    let request = TestRequest::parse(&args.args)?;
    let root = &ctx.root;
    let test = &ctx.config.test;

    let block = std::env::var(PR_TESTS_ENV).ok();
    let plan = plan(test, root, &request, block.as_deref())?;
    if let Some(output) = &request.selection_output {
        plan.write_selection(&root.join(output))?;
    }
    match plan {
        Plan::Lane(lane_name) => run_lane(test, root, lane_name, &request),
        Plan::NotRequired(lane_name) => {
            println!("{lane_name}: skipped: not required");
            Ok(())
        }
        Plan::Draft { lane, scope } => {
            let mut command = draft_command(test, lane, &scope, &request)?;
            execute(test, root, lane, &mut command)
        }
    }
}

/// Whether the test command given `args` runs any lane: a `--draft` run of a
/// lane that holds none of the draft scope `pr_tests` completes runs nothing.
///
/// # Errors
///
/// Fails when `args` are not a valid request, the test configuration is
/// invalid, or the draft scope cannot be read.
pub fn selects_any_lane(
    root: &Path,
    project: &ProjectConfig,
    args: &[String],
    pr_tests: Option<&str>,
) -> Result<bool> {
    let request = TestRequest::parse(args)?;
    Ok(match plan(&project.test, root, &request, pr_tests)? {
        Plan::NotRequired(_) => false,
        Plan::Lane(_) | Plan::Draft { .. } => true,
    })
}

/// The lanes a request runs.
enum Plan<'a> {
    /// One lane, whole.
    Lane(&'a str),
    /// One lane's share of the draft scope.
    Draft { lane: &'a str, scope: DraftScope },
    /// A draft lane that holds none of the draft scope.
    NotRequired(&'a str),
}

impl Plan<'_> {
    fn write_selection(&self, path: &Path) -> Result<()> {
        let lanes: Vec<&str> = match self {
            Self::Lane(lane) | Self::Draft { lane, .. } => vec![lane],
            Self::NotRequired(_) => Vec::new(),
        };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        fs::write(path, serde_json::to_vec(&lanes)?)
            .with_context(|| format!("writing {}", path.display()))
    }
}

fn plan<'a>(
    test: &'a TestCommandConfig,
    root: &Path,
    request: &'a TestRequest,
    pr_tests: Option<&str>,
) -> Result<Plan<'a>> {
    validate_config(test)?;
    let lane = select_lane(test, request)?;
    if !request.draft {
        return Ok(Plan::Lane(lane));
    }
    let scope = DraftScope::load(test, root, pr_tests)?;
    let Some(config) = test
        .lanes
        .get(lane)
        .filter(|config| scope.holds(&config.cargo))
    else {
        return Ok(Plan::NotRequired(lane));
    };
    Ok(match config.runner {
        TestRunner::Nextest(_) => Plan::Draft { lane, scope },
        TestRunner::Cargo(_) => Plan::Lane(lane),
    })
}

/// Reports build time before the verdict rather than after it: a red lane is exactly when the
/// build's share of the wall clock needs explaining, and reporting after an early return would
/// print the number only for lanes that passed.
pub(super) fn run_lane(
    test: &TestCommandConfig,
    root: &Path,
    lane_name: &str,
    request: &TestRequest,
) -> Result<()> {
    let lane = requested_lane(test, lane_name, request)?;
    if let Some(path) = &request.expected_tests {
        admit_expected(&lane, &request.passthrough, root, &root.join(path))?;
    }
    let mut command = lane.command(NextestAction::Run, &request.passthrough)?;
    execute(test, root, lane_name, &mut command)
}

/// Runs a lane's command and judges it by its report as well as its status.
pub(super) fn execute(
    test: &TestCommandConfig,
    root: &Path,
    lane_name: &str,
    command: &mut Command,
) -> Result<()> {
    command.current_dir(root);
    let evidence = Evidence::of(root, test, command)?;
    evidence.clear();
    let status = command.status().with_context(|| {
        format!(
            "failed to run test lane `{lane_name}`: {}",
            command.get_program().to_string_lossy()
        )
    })?;
    evidence.verdict(lane_name, &test.known_flakes, status.code())
}

/// The lane as `request` asks for it, narrowed to the request's filterset.
pub(super) fn requested_lane(
    test: &TestCommandConfig,
    lane_name: &str,
    request: &TestRequest,
) -> Result<ResolvedLane> {
    let lane = resolve(test, &requested(test, lane_name, Some(request))?)?;
    match &request.narrow {
        Some(filterset) => lane.filtered(filterset.clone()),
        None => Ok(lane),
    }
}

/// The command of one draft lane: the lane's own command with one added
/// filterset, the draft scope, and a pass when the lane holds none of it.
fn draft_command(
    test: &TestCommandConfig,
    lane_name: &str,
    scope: &DraftScope,
    request: &TestRequest,
) -> Result<Command> {
    let mut extra = request.passthrough.clone();
    extra.push("--no-tests=pass".to_owned());
    requested_lane(test, lane_name, request)?
        .filtered(scope.filterset())?
        .command(NextestAction::Run, &extra)
}

/// Operation on the same configured nextest selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NextestAction {
    Run,
    List,
}

/// Build the command for a configured test lane.
/// # Errors
/// Returns an error when the lane or its backend configuration is invalid.
pub fn nextest_command_for_lane(
    project: &ProjectConfig,
    lane_name: &str,
    extra: &[String],
    action: NextestAction,
) -> Result<Command> {
    let test = &project.test;
    resolve(test, &requested(test, lane_name, None)?)?.command(action, extra)
}

#[cfg(test)]
mod selection_tests {
    use super::*;
    use crate::common::project::{
        TestCargoOptions, TestCargoRunner, TestLaneConfig, TestNetBackendConfig, TestRequiredConfig,
    };

    fn args_of(command: &Command) -> Vec<String> {
        command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    fn request(args: &[&str]) -> TestRequest {
        TestRequest::parse(&args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>()).unwrap()
    }

    /// A lane that builds the required `xtask`, and one that builds none of
    /// the required packages.
    fn drafted() -> TestCommandConfig {
        let lane = |packages: &[&str]| TestLaneConfig {
            cargo: TestCargoOptions {
                packages: packages.iter().map(|name| (*name).to_owned()).collect(),
                ..TestCargoOptions::default()
            },
            ..TestLaneConfig::default()
        };
        TestCommandConfig {
            lanes: [
                ("tooling".to_owned(), lane(&["xtask", "kithara-devtools"])),
                ("harness".to_owned(), lane(&["kithara-test-utils"])),
            ]
            .into_iter()
            .collect(),
            net_backends: [("none".to_owned(), TestNetBackendConfig::default())]
                .into_iter()
                .collect(),
            default_backend: "none".to_owned(),
            default_lane: "tooling".to_owned(),
            nextest_config: ".config/nextest.toml".to_owned(),
            required: TestRequiredConfig {
                packages: vec!["xtask".to_owned()],
            },
            ..TestCommandConfig::default()
        }
    }

    /// A draft lane runs exactly what the default branch runs, with one
    /// filterset for the draft scope added and an empty selection allowed.
    #[test]
    fn a_draft_lane_runs_its_own_command_with_one_filter_for_the_draft_scope() {
        let test = drafted();
        let root = crate::test::repository_tests::root();
        let main = request(&["--lane=tooling", "--profile", "ci"]);
        let draft = request(&["--lane=tooling", "--draft", "--profile", "ci"]);

        let main_args = args_of(
            &requested_lane(&test, "tooling", &main)
                .unwrap()
                .command(NextestAction::Run, &main.passthrough)
                .unwrap(),
        );
        let Plan::Draft { lane, scope } = plan(&test, &root, &draft, None).unwrap() else {
            panic!("a lane that builds a required package runs its share of the draft");
        };
        let draft_args = args_of(&draft_command(&test, lane, &scope, &draft).unwrap());

        let filter = draft_args.iter().position(|arg| arg == "-E").unwrap();
        assert_eq!(draft_args[filter + 1], "package(xtask)");
        assert_eq!(draft_args.iter().filter(|arg| *arg == "-E").count(), 1);
        assert_eq!(
            draft_args.last().map(String::as_str),
            Some("--no-tests=pass")
        );
        let mut rest = draft_args.clone();
        rest.pop();
        rest.drain(filter..=filter + 1);
        assert_eq!(rest, main_args);
    }

    #[test]
    fn a_draft_lane_that_holds_no_required_package_is_not_required() {
        let test = drafted();
        let root = crate::test::repository_tests::root();
        let args = ["--lane=harness".to_owned(), "--draft".to_owned()];

        assert!(matches!(
            plan(&test, &root, &request(&["--lane=harness", "--draft"]), None).unwrap(),
            Plan::NotRequired("harness")
        ));
        let project = ProjectConfig {
            test,
            ..ProjectConfig::default()
        };
        assert!(!selects_any_lane(&root, &project, &args, None).unwrap());
        assert!(
            selects_any_lane(
                &root,
                &project,
                &args,
                Some("packages = [\"kithara-test-utils\"]")
            )
            .unwrap()
        );
    }

    #[test]
    fn a_draft_cargo_suite_keeps_its_command_when_the_scope_admits_it() {
        let mut test = drafted();
        let config = test.lanes.get_mut("harness").expect("fixture lane");
        config.cargo.profile = Some("test-release".to_owned());
        config.runner = TestRunner::Cargo(TestCargoRunner {
            name_filters: vec!["selenium".to_owned()],
            no_capture: true,
            ..TestCargoRunner::default()
        });
        config
            .env
            .insert("KITHARA_SELENIUM_BROWSER".to_owned(), "firefox".to_owned());
        let root = crate::test::repository_tests::root();
        let draft = request(&["--lane=harness", "--draft", "--offline"]);
        assert!(matches!(
            plan(&test, &root, &draft, None).expect("valid scope"),
            Plan::NotRequired("harness")
        ));
        for block in [
            r#"filter = "all()""#,
            r#"packages = ["kithara-test-utils"]"#,
            r#"filter = "test(seek)""#,
        ] {
            let command = match plan(&test, &root, &draft, Some(block)).expect("valid scope") {
                Plan::Lane(lane) => requested_lane(&test, lane, &draft)
                    .expect("resolved lane")
                    .command(NextestAction::Run, &draft.passthrough),
                Plan::Draft { lane, scope } => draft_command(&test, lane, &scope, &draft),
                Plan::NotRequired(_) => panic!("the scope admits this suite"),
            }
            .expect("an admitted cargo suite runs its configured command");
            assert_eq!(command.get_program(), "cargo");
            assert_eq!(
                args_of(&command),
                [
                    "test",
                    "-p",
                    "kithara-test-utils",
                    "--profile",
                    "test-release",
                    "--offline",
                    "--",
                    "selenium",
                    "--nocapture"
                ]
            );
            assert!(
                command
                    .get_envs()
                    .any(|(key, value)| key == "KITHARA_SELENIUM_BROWSER"
                        && value == Some(std::ffi::OsStr::new("firefox")))
            );
        }
        let narrow = request(&["--lane=harness", "--draft", "--narrow=test(seek)"]);
        assert!(requested_lane(&test, "harness", &narrow).is_err());
    }

    #[test]
    fn selection_output_names_the_lanes_in_the_execution_plan() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("reports/selection.json");
        for (plan, expected) in [
            (Plan::Lane("workspace"), vec!["workspace"]),
            (Plan::NotRequired("workspace"), vec![]),
        ] {
            plan.write_selection(&path).unwrap();
            let selected: Vec<String> = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            assert_eq!(selected, expected);
        }
    }
}

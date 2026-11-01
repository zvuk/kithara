use std::{fmt::Display, fs, path::Path, process::Command};

use anyhow::{Context, Result};
use clap::Args;

use super::{
    inventory::admit_expected,
    request::TestRequest,
    resolve::{ResolvedLane, resolve},
    selection::{requested, select_lane, validate_config},
};
use crate::{
    Ctx,
    common::project::{ProjectConfig, TestCommandConfig},
    retried::Evidence,
    touched::{self, Touched},
    verdict::ChildFailure,
};

#[derive(Debug, Args)]
#[command(trailing_var_arg = true)]
pub struct TestArgs {
    /// Arguments for the configured test command. Recipe-level flags accepted anywhere:
    /// `--lane=<configured-name>`, `--touched`, `--flash=true|false|on|off`, `--no-flash`,
    /// `--loom=true|false|on|off`, `--no-loom`, `--no-block=true|false|on|off`,
    /// `--load=true|false|on|off`, `--net-backend=<configured-name>`, `--narrow=<filterset>`,
    /// `--selection-output=<path>` (JSON lane names), and `--expected-tests=<path>`
    /// (UTF-8 TSV: binary ID, tab, full test name per line).
    #[arg(value_name = "ARGS", allow_hyphen_values = true)]
    pub(crate) args: Vec<String>,
}

pub(crate) fn run(args: &TestArgs, ctx: &Ctx) -> Result<()> {
    let request = TestRequest::parse(&args.args)?;
    let root = &ctx.root;
    let test = &ctx.config.test;

    let base = touched::base_branch(&|name| std::env::var_os(name));
    let plan = plan(test, root, &request, &base)?;
    if let Some(output) = &request.selection_output {
        plan.write_selection(&root.join(output))?;
    }
    match plan {
        Plan::Touched(selected) if selected.is_empty() => {
            println!("no owned path touched; the nightly sweep covers these lanes");
            Ok(())
        }
        Plan::Touched(selected) => run_each(&selected, |run| {
            let mut command = touched_command(test, run, &request)?;
            execute(test, root, run.lane(), &mut command)
        }),
        Plan::Lane(lane_name) => run_lane(test, root, lane_name, &request),
    }
}

/// Whether the test command given `args` runs any lane: a `--touched` run
/// whose branch touched none of its lanes since it left `base` runs nothing.
///
/// # Errors
///
/// Fails when `args` are not a valid request, the test configuration is
/// invalid, or git cannot say what the branch touched.
pub fn selects_any_lane(
    root: &Path,
    project: &ProjectConfig,
    args: &[String],
    base: &str,
) -> Result<bool> {
    let request = TestRequest::parse(args)?;
    Ok(match plan(&project.test, root, &request, base)? {
        Plan::Touched(selected) => !selected.is_empty(),
        Plan::Lane(_) => true,
    })
}

/// The lanes a request runs.
enum Plan<'a> {
    /// The runs the branch touched, possibly none.
    Touched(Vec<Touched>),
    /// One lane, whole.
    Lane(&'a str),
}

impl Plan<'_> {
    fn write_selection(&self, path: &Path) -> Result<()> {
        let lanes: Vec<&str> = match self {
            Self::Touched(selected) => selected.iter().map(Touched::lane).collect(),
            Self::Lane(lane) => vec![lane],
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
    base: &str,
) -> Result<Plan<'a>> {
    validate_config(test)?;
    if request.touched {
        return Ok(Plan::Touched(touched::lanes(
            test,
            root,
            base,
            &request.lanes,
        )?));
    }
    select_lane(test, request).map(Plan::Lane)
}

/// Run `selected` serially without letting the first failure hide the rest:
/// the error names each red lane with its own reason and leaves with the
/// first one's exit code.
pub(super) fn run_each<T, F>(selected: &[T], mut run: F) -> Result<()>
where
    T: Display,
    F: FnMut(&T) -> Result<()>,
{
    let mut failures = Vec::new();
    let mut code = None;
    for lane in selected {
        println!("=== {lane} ===");
        if let Err(error) = run(lane) {
            code.get_or_insert_with(|| {
                error
                    .downcast_ref::<ChildFailure>()
                    .map_or(1, ChildFailure::exit_code)
            });
            failures.push(format!("{error:#}"));
        }
    }
    code.map_or(Ok(()), |code| {
        Err(ChildFailure::explained(
            "touched test lanes".to_owned(),
            Some(code),
            failures.join("\n"),
        ))
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

pub(super) fn lane_command(
    test: &TestCommandConfig,
    lane_name: &str,
    request: &TestRequest,
) -> Result<Command> {
    requested_lane(test, lane_name, request)?.command(NextestAction::Run, &request.passthrough)
}

/// The lane as `request` asks for it, narrowed to the request's filterset.
fn requested_lane(
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

/// The command of one touched run: a lane whole, or narrowed to packages.
pub(super) fn touched_command(
    test: &TestCommandConfig,
    run: &Touched,
    request: &TestRequest,
) -> Result<Command> {
    match run {
        Touched::Whole(lane_name) => lane_command(test, lane_name, request),
        Touched::Narrowed { lane, packages } => requested_lane(test, lane, request)?
            .narrowed(packages)?
            .command(NextestAction::Run, &request.passthrough),
    }
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

    #[test]
    fn selection_output_names_the_lanes_in_the_execution_plan() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("reports/selection.json");
        for (plan, expected) in [
            (Plan::Touched(Vec::new()), vec![]),
            (Plan::Lane("workspace"), vec!["workspace"]),
            (
                Plan::Touched(vec![
                    Touched::Whole("audio".to_owned()),
                    Touched::Narrowed {
                        lane: "workspace".to_owned(),
                        packages: ["kithara-net".to_owned()].into_iter().collect(),
                    },
                ]),
                vec!["audio", "workspace"],
            ),
        ] {
            plan.write_selection(&path).unwrap();
            let selected: Vec<String> = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            assert_eq!(selected, expected);
        }
    }
}

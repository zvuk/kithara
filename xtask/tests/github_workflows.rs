use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use proc_macro2::{Delimiter, TokenStream, TokenTree};
use serde_yaml_ng::{Mapping, Value};
use syn::{
    BinOp, Expr, ItemConst, Meta, Stmt, Token, parse::Parser, punctuated::Punctuated, visit::Visit,
};

const STRESS_RAW_DIR: &str = "${{ runner.temp }}/kithara-stress/raw";
const HEAVY_LINUX_GROUP: &str = "heavy-linux-${{ github.repository }}";
const STRESS_EXECUTE_COMMAND: &str = r#"args=(
  --subject-root "$GITHUB_WORKSPACE/subject"
  --output "$RUNNER_TEMP/kithara-stress/raw"
  --expected-controller-sha "$CONTROLLER_SHA"
  --expected-subject-sha "$SUBJECT_SHA"
  --shard-index "$SHARD_INDEX"
  --shard-count 3
)
[[ -z "$FILTER" ]] || args+=(--filter "$FILTER")
[[ -z "$COUNT" ]] || args+=(--count "$COUNT")
# `--mode` and `--lane` repeat per entry; each input is a space-separated
# list, so one flag carrying the whole string would name a mode or a lane
# that does not exist and fail the run at argument parsing.
for mode in $MODE; do args+=(--mode "$mode"); done
for lane in $LANE; do args+=(--lane "$lane"); done
just ci stress "${args[@]}""#;
const STRESS_REPORT_COMMAND: &str = r#"args=(
  --raw "$GITHUB_WORKSPACE/raw/shard-0"
  --raw "$GITHUB_WORKSPACE/raw/shard-1"
  --raw "$GITHUB_WORKSPACE/raw/shard-2"
  --output "$GITHUB_WORKSPACE/target/stress-report.md"
  --expected-controller-sha "$CONTROLLER_SHA"
  --expected-subject-sha "$SUBJECT_SHA"
  --execute-result "$EXECUTE_RESULT"
)
[[ -z "$FILTER" ]] || args+=(--filter "$FILTER")
[[ -z "$COUNT" ]] || args+=(--count "$COUNT")
for mode in $MODE; do args+=(--mode "$mode"); done
for lane in $LANE; do args+=(--lane "$lane"); done
just ci stress-report "${args[@]}""#;

const AUTHORIZATION_SCRIPT: &str = r#"python3 - <<'PY'
import json
import os
import sys

actor = os.environ["ACTOR"]
owner = os.environ["OWNER"]
if actor != owner:
    print(f"CI may only be started by repository owner {owner!r}, got {actor!r}")
    sys.exit(1)

raw_labels = os.environ.get("RUNNER_LABELS", "")
try:
    labels = json.loads(raw_labels)
except json.JSONDecodeError as error:
    print(f"KITHARA_RUNNER_LABELS is not valid JSON: {error}")
    sys.exit(1)
if not isinstance(labels, list) or not labels or not all(
    isinstance(label, str) and label for label in labels
):
    print("KITHARA_RUNNER_LABELS must be a non-empty JSON array of non-empty strings")
    sys.exit(1)
PY"#;

const REQUIRED_SCRIPT: &str = r#"python3 - <<'PY'
import json
import os
import sys

results = json.loads(os.environ["RESULTS"])
required_lanes = set(os.environ["REQUIRED_LANES"].split())
optional_required = bool(required_lanes)
ui_required = "all" in required_lanes or "deep-ui" in required_lanes or os.environ["TOUCHED_UI_REQUIRED"] == "true"
android_required = "all" in required_lanes or "android-test" in required_lanes
incomplete = {
    name: job["result"]
    for name, job in results.items()
    if job["result"] != "success"
    and not (name in {"deep", "platforms", "quality"} and job["result"] == "skipped" and not optional_required)
    and not (name == "ui" and job["result"] == "skipped" and not ui_required)
    and not (name == "android" and job["result"] == "skipped" and not android_required)
}
if incomplete:
    print(f"required CI jobs did not execute successfully: {incomplete}")
    sys.exit(1)
PY"#;

fn github_workflow(name: &str) -> Value {
    let text = github_workflow_text(name);
    serde_yaml_ng::from_str(&text).expect("workflow is valid YAML")
}

fn github_workflow_text(name: &str) -> String {
    fs::read_to_string(workflows_dir().join(name)).expect("workflow is readable")
}

fn workflows_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask has a workspace root")
        .join(".github/workflows")
}

fn workflow_file_names() -> BTreeSet<String> {
    fs::read_dir(workflows_dir())
        .expect("workflow directory is readable")
        .map(|entry| {
            entry
                .expect("workflow entry is readable")
                .file_name()
                .to_str()
                .expect("workflow file name is UTF-8")
                .to_owned()
        })
        .filter(|name| name.ends_with(".yml"))
        .collect()
}

fn configured_stress_prekill_secs(root: &Path) -> u64 {
    let config: toml::Value = toml::from_str(
        &fs::read_to_string(root.join(".config/xtask.toml")).expect("xtask config is readable"),
    )
    .expect("xtask config is valid TOML");
    let modes = config["stress"]["modes"]
        .as_table()
        .expect("stress modes are configured");
    let values = modes
        .values()
        .filter_map(|mode| mode.get("set_env"))
        .filter_map(toml::Value::as_table)
        .filter_map(|environment| environment.get("KITHARA_HANG_PREKILL_SECS"))
        .filter_map(toml::Value::as_str)
        .map(|value| value.parse::<u64>().expect("pre-kill value is seconds"))
        .collect::<BTreeSet<_>>();
    assert_eq!(values.len(), 1, "stress modes disagree on pre-kill timing");
    values
        .into_iter()
        .next()
        .expect("pre-kill timing is configured")
}

fn mapping_field<'a>(mapping: &'a Mapping, name: &str) -> &'a Value {
    mapping
        .get(name)
        .unwrap_or_else(|| panic!("YAML mapping has no `{name}` field"))
}

fn workflow_jobs(workflow: &Value) -> &Mapping {
    mapping_field(
        workflow.as_mapping().expect("workflow is a mapping"),
        "jobs",
    )
    .as_mapping()
    .expect("workflow jobs are a mapping")
}

fn workflow_concurrency(workflow: &Value) -> &Mapping {
    mapping_field(
        workflow.as_mapping().expect("workflow is a mapping"),
        "concurrency",
    )
    .as_mapping()
    .expect("workflow concurrency is a mapping")
}

fn workflow_job<'a>(jobs: &'a Mapping, name: &str) -> &'a Mapping {
    mapping_field(jobs, name)
        .as_mapping()
        .unwrap_or_else(|| panic!("workflow job `{name}` must be a mapping"))
}

fn workflow_job_names(jobs: &Mapping) -> BTreeSet<String> {
    jobs.keys()
        .map(|name| {
            name.as_str()
                .expect("workflow job name is a string")
                .to_owned()
        })
        .collect()
}

fn job_needs(job: &Mapping) -> BTreeSet<String> {
    match mapping_field(job, "needs") {
        Value::String(name) => BTreeSet::from([name.clone()]),
        Value::Sequence(names) => names
            .iter()
            .map(|name| {
                name.as_str()
                    .expect("workflow dependency is a string")
                    .to_owned()
            })
            .collect(),
        _ => panic!("workflow job dependencies must be a string or sequence"),
    }
}

fn first_step(job: &Mapping) -> &Mapping {
    mapping_field(job, "steps")
        .as_sequence()
        .expect("workflow steps are a sequence")
        .first()
        .expect("workflow has at least one step")
        .as_mapping()
        .expect("workflow step is a mapping")
}

fn named_step<'a>(job: &'a Mapping, name: &str) -> &'a Mapping {
    mapping_field(job, "steps")
        .as_sequence()
        .expect("workflow steps are a sequence")
        .iter()
        .find_map(|step| {
            let step = step.as_mapping()?;
            (step.get("name").and_then(Value::as_str) == Some(name)).then_some(step)
        })
        .unwrap_or_else(|| panic!("workflow job has no `{name}` step"))
}

fn job_step_names(job: &Mapping) -> BTreeSet<String> {
    mapping_field(job, "steps")
        .as_sequence()
        .expect("workflow steps are a sequence")
        .iter()
        .map(|step| {
            mapping_field(
                step.as_mapping().expect("workflow step is a mapping"),
                "name",
            )
            .as_str()
            .expect("workflow step name is a string")
            .to_owned()
        })
        .collect()
}

fn assert_action_repository(step: &Mapping, repository: &str) {
    let uses = mapping_field(step, "uses")
        .as_str()
        .expect("action reference is a string");
    assert_eq!(uses.split_once('@').map(|(name, _)| name), Some(repository));
}

fn assert_checkout(step: &Mapping, path: &str, repository: &str, revision: &str) {
    assert_action_repository(step, "actions/checkout");
    let inputs = mapping_field(step, "with")
        .as_mapping()
        .expect("checkout inputs are a mapping");
    assert_eq!(mapping_field(inputs, "path").as_str(), Some(path));
    assert_eq!(
        mapping_field(inputs, "repository").as_str(),
        Some(repository)
    );
    assert_eq!(mapping_field(inputs, "ref").as_str(), Some(revision));
    assert_eq!(
        mapping_field(inputs, "persist-credentials").as_bool(),
        Some(false)
    );
}

fn step_position(job: &Mapping, name: &str) -> usize {
    mapping_field(job, "steps")
        .as_sequence()
        .expect("workflow steps are a sequence")
        .iter()
        .position(|step| {
            step.as_mapping()
                .and_then(|step| step.get("name"))
                .and_then(Value::as_str)
                == Some(name)
        })
        .unwrap_or_else(|| panic!("workflow job has no `{name}` step"))
}

fn assert_always(step: &Mapping) {
    assert_eq!(mapping_field(step, "if").as_str(), Some("always()"));
}

fn assert_uploads(step: &Mapping, paths: &[&str]) {
    assert_always(step);
    assert_action_repository(step, "actions/upload-artifact");
    let with = mapping_field(step, "with")
        .as_mapping()
        .expect("artifact inputs are a mapping");
    let actual: BTreeSet<&str> = mapping_field(with, "path")
        .as_str()
        .expect("artifact paths are a string")
        .lines()
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .collect();
    assert_eq!(actual, paths.iter().copied().collect());
}

fn assert_no_key(value: &Value, forbidden: &str) {
    match value {
        Value::Sequence(values) => {
            for value in values {
                assert_no_key(value, forbidden);
            }
        }
        Value::Mapping(mapping) => {
            for (key, value) in mapping {
                assert_ne!(
                    key.as_str(),
                    Some(forbidden),
                    "workflow contains forbidden `{forbidden}`"
                );
                assert_no_key(value, forbidden);
            }
        }
        _ => {}
    }
}

fn assert_hosted_authorization(job: &Mapping) {
    assert_eq!(
        mapping_field(job, "runs-on").as_str(),
        Some("ubuntu-latest")
    );

    let step = first_step(job);
    let env = mapping_field(step, "env")
        .as_mapping()
        .expect("authorization environment is a mapping");
    assert_eq!(
        mapping_field(env, "ACTOR").as_str(),
        Some("${{ github.actor }}")
    );
    assert_eq!(
        mapping_field(env, "OWNER").as_str(),
        Some("${{ github.repository_owner }}")
    );
    assert_eq!(
        mapping_field(env, "RUNNER_LABELS").as_str(),
        Some("${{ vars.KITHARA_RUNNER_LABELS }}")
    );
    assert_eq!(
        mapping_field(step, "run")
            .as_str()
            .expect("authorization step is a script")
            .trim(),
        AUTHORIZATION_SCRIPT
    );
}

/// Both entry points of `ci.yml` take the same two inputs, and the lane
/// selector defaults to every lane.
fn assert_ci_entry_inputs(workflow: &Value) {
    let root = workflow.as_mapping().expect("workflow is a mapping");
    let triggers = mapping_field(root, "on")
        .as_mapping()
        .expect("workflow triggers are a mapping");
    for trigger in ["workflow_call", "workflow_dispatch"] {
        let inputs = mapping_field(
            mapping_field(triggers, trigger)
                .as_mapping()
                .unwrap_or_else(|| panic!("{trigger} is a mapping")),
            "inputs",
        )
        .as_mapping()
        .expect("workflow inputs are a mapping");
        assert_eq!(
            inputs
                .keys()
                .map(|name| name.as_str().expect("input name is a string"))
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["fixture-refresh", "required_lanes"])
        );
        let required_lanes = mapping_field(inputs, "required_lanes")
            .as_mapping()
            .expect("required_lanes is a mapping");
        assert_eq!(
            mapping_field(required_lanes, "type").as_str(),
            Some("string")
        );
        assert_eq!(mapping_field(required_lanes, "default").as_str(), Some(""));
    }
}

#[test]
fn github_ci_is_fail_closed_and_aggregates_every_job() {
    let workflow = github_workflow("ci.yml");
    assert_ci_entry_inputs(&workflow);
    let concurrency = workflow_concurrency(&workflow);
    assert_eq!(
        mapping_field(concurrency, "group").as_str(),
        Some("ci-${{ github.ref }}")
    );
    // One queue per branch: a newer push supersedes obsolete work from the same
    // branch while pushes to different branches remain independent.
    assert_eq!(
        mapping_field(concurrency, "cancel-in-progress").as_bool(),
        Some(true)
    );
    assert!(
        !concurrency.contains_key("queue"),
        "GitHub concurrency has no queue extension"
    );
    let jobs = workflow_jobs(&workflow);

    let authorize = workflow_job(jobs, "authorize");
    assert_hosted_authorization(authorize);
    // The one condition this job may carry. Anything else here, and a push that
    // should have been judged is skipped instead — reported as a green run. Both
    // halves are load-bearing: the variable admits a repository that declares
    // runners, the fork flag admits one whose settings this repository cannot
    // read.
    assert_eq!(
        mapping_field(authorize, "if").as_str(),
        Some("github.event.repository.fork || vars.KITHARA_RUNNER_LABELS != ''")
    );
    let gate = workflow_job(jobs, "gate");
    assert_eq!(job_needs(gate), BTreeSet::from(["authorize".to_owned()]));
    assert_eq!(
        mapping_field(gate, "runs-on").as_str(),
        Some("${{ fromJSON(vars.KITHARA_RUNNER_LABELS) }}")
    );

    let (selection_name, _) = ui_selection(jobs);
    for name in workflow_job_names(jobs) {
        if matches!(name.as_str(), "authorize" | "gate" | "required") {
            continue;
        }
        let job = workflow_job(jobs, &name);
        assert_no_key(&Value::Mapping(job.clone()), "continue-on-error");
        let expected_needs = if name == selection_name {
            job_needs(gate)
        } else if name == "ui" {
            BTreeSet::from(["gate".to_owned(), selection_name.to_owned()])
        } else {
            BTreeSet::from(["gate".to_owned()])
        };
        assert_eq!(
            job_needs(job),
            expected_needs,
            "workflow job `{name}` bypasses the self-hosted gate"
        );
        let condition = job.get("if").and_then(Value::as_str).unwrap_or_default();
        assert!(!condition.contains("KITHARA_RUNNER_LABELS"));
        assert!(!condition.contains("github.actor"));
    }

    // The push entry describes no lane of its own. It names a role, and the
    // catalog answers which lanes that is - so a gate lane cannot be declared
    // here and nowhere else, which is how GitHub and GitLab drifted apart.
    let lanes = workflow_job(jobs, "lanes");
    assert_eq!(
        mapping_field(lanes, "uses").as_str(),
        Some("./.github/workflows/run.yml")
    );
    let with = mapping_field(lanes, "with")
        .as_mapping()
        .expect("the gate call passes inputs");
    assert_eq!(mapping_field(with, "role").as_str(), Some("gate"));
    assert!(
        mapping_field(with, "only")
            .as_str()
            .expect("gate passes the selector")
            .contains("inputs.required_lanes")
    );
    for (name, role) in [
        ("deep", "deep"),
        ("platforms", "platforms"),
        ("quality", "quality"),
    ] {
        let job = workflow_job(jobs, name);
        let with = mapping_field(job, "with")
            .as_mapping()
            .unwrap_or_else(|| panic!("{name} passes inputs"));
        assert_eq!(mapping_field(with, "role").as_str(), Some(role));
        assert!(
            mapping_field(with, "only")
                .as_str()
                .unwrap_or_else(|| panic!("{name} passes the selector"))
                .contains("inputs.required_lanes")
        );
    }
    for name in workflow_job_names(jobs) {
        let job = workflow_job(jobs, &name);
        assert_no_key(&Value::Mapping(job.clone()), "strategy");
    }

    // The Android baseline needs the emulator pool, so a push starts it by name
    // rather than through the shared fan-out.
    let android = workflow_job(jobs, "android");
    assert_eq!(
        mapping_field(android, "uses").as_str(),
        Some("./.github/workflows/android.yml")
    );
    assert!(
        mapping_field(android, "if")
            .as_str()
            .expect("android carries a condition")
            .contains("' android-test '")
    );

    let required = workflow_job(jobs, "required");
    assert_eq!(
        mapping_field(required, "runs-on").as_str(),
        Some("ubuntu-latest")
    );
    // The aggregate demands success from every job it needs, and a skip is not a
    // success, so it has to carry the same guard verbatim or it alone stays red
    // on a repository the run skipped.
    assert_eq!(
        mapping_field(required, "if").as_str(),
        Some(
            "${{ always() && (github.event.repository.fork || vars.KITHARA_RUNNER_LABELS != '') }}"
        )
    );
    let mut expected = workflow_job_names(jobs);
    expected.remove("required");
    assert_eq!(job_needs(required), expected);

    let step = first_step(required);
    let env = mapping_field(step, "env")
        .as_mapping()
        .expect("required environment is a mapping");
    assert_eq!(
        mapping_field(env, "RESULTS").as_str(),
        Some("${{ toJSON(needs) }}")
    );
    assert_eq!(
        mapping_field(env, "REQUIRED_LANES").as_str(),
        Some(
            "${{ github.ref == format('refs/heads/{0}', github.event.repository.default_branch) && 'all' || inputs.required_lanes || '' }}"
        )
    );
    assert_eq!(
        mapping_field(step, "run")
            .as_str()
            .expect("required step is a script")
            .trim(),
        REQUIRED_SCRIPT
    );
}

// A push to a branch whose pull request is ready for review runs the suite
// whole, a draft's push only what it touched. Nothing here reacts to a pull
// request, so the push asks: the hosted job looks the request up before any
// job reaches the fleet, and every role reads the one kind it named instead of
// deciding again.
#[test]
fn a_ready_pull_request_is_a_merge_request_pipeline() {
    let workflow = github_workflow("ci.yml");
    let jobs = workflow_jobs(&workflow);

    let authorize = workflow_job(jobs, "authorize");
    let outputs = mapping_field(authorize, "outputs")
        .as_mapping()
        .expect("authorization names outputs");
    assert_eq!(
        mapping_field(outputs, "kind").as_str(),
        Some("${{ steps.kind.outputs.kind }}")
    );
    let step = named_step(authorize, "Name the pipeline kind");
    assert_eq!(mapping_field(step, "id").as_str(), Some("kind"));
    let env = mapping_field(step, "env")
        .as_mapping()
        .expect("the kind step reads its inputs from the environment");
    assert_eq!(
        mapping_field(env, "GH_TOKEN").as_str(),
        Some("${{ github.token }}")
    );
    let script = mapping_field(step, "run")
        .as_str()
        .expect("the kind step is a script");
    for named in [
        "main",
        "merge-request",
        "branch",
        ".draft",
        ".parent.full_name",
    ] {
        assert!(
            script.contains(named),
            "the kind step never reads `{named}`"
        );
    }

    let gate = workflow_job(jobs, "gate");
    let outputs = mapping_field(gate, "outputs")
        .as_mapping()
        .expect("the gate hands the kind on");
    assert_eq!(
        mapping_field(outputs, "kind").as_str(),
        Some("${{ needs.authorize.outputs.kind }}")
    );
    for name in ["lanes", "deep", "platforms", "quality"] {
        let with = mapping_field(workflow_job(jobs, name), "with")
            .as_mapping()
            .unwrap_or_else(|| panic!("{name} passes inputs"));
        assert_eq!(
            mapping_field(with, "kind").as_str(),
            Some("${{ needs.gate.outputs.kind }}"),
            "role `{name}` decides the pipeline kind again"
        );
    }
}

// One entry reacts to every push. Optional suites belong inside that run so a
// commit has one verdict rather than independent CI and UI results.
#[test]
fn the_gate_is_the_only_workflow_every_push_starts() {
    let mut entries = Vec::new();
    for name in workflow_file_names() {
        let workflow = github_workflow(&name);
        let on = mapping_field(workflow.as_mapping().expect("workflow is a mapping"), "on");
        if on
            .as_mapping()
            .and_then(|on| on.get("push"))
            .is_some_and(Value::is_null)
        {
            entries.push(name);
        }
    }
    assert_eq!(
        entries,
        vec!["ci.yml".to_owned()],
        "a push starts more than the gate"
    );
}

fn concurrency_prefixes(owner: &Mapping) -> BTreeSet<String> {
    let Some(concurrency) = owner.get("concurrency") else {
        return BTreeSet::new();
    };
    let group = match concurrency {
        Value::String(group) => group.as_str(),
        Value::Mapping(mapping) => mapping_field(mapping, "group")
            .as_str()
            .expect("concurrency group is a string"),
        _ => panic!("concurrency is a string or a mapping"),
    };
    let formatted: BTreeSet<String> = group
        .match_indices("format('")
        .map(|(index, marker)| {
            let literal = group[index + marker.len()..]
                .split('\'')
                .next()
                .expect("format template is quoted");
            group_prefix(literal)
        })
        .collect();
    if formatted.is_empty() {
        BTreeSet::from([group_prefix(group)])
    } else {
        formatted
    }
}

#[test]
fn a_queue_is_never_declared_beside_a_cancellation_that_can_fire() {
    let mut conflicts = Vec::new();
    for name in workflow_file_names() {
        let workflow = github_workflow(&name);
        let mut owners = vec![("".to_owned(), workflow.as_mapping().cloned())];
        for (job_name, job) in workflow_jobs(&workflow) {
            let job_name = job_name.as_str().expect("workflow job name is a string");
            owners.push((format!(" job `{job_name}`"), job.as_mapping().cloned()));
        }
        for (where_, owner) in owners {
            let Some(Value::Mapping(concurrency)) =
                owner.as_ref().and_then(|owner| owner.get("concurrency"))
            else {
                continue;
            };
            if concurrency.get("queue").and_then(Value::as_str) != Some("max") {
                continue;
            }
            // GitHub rejects `queue: max` beside `cancel-in-progress: true`, and
            // an expression that reads false on a fork still reads true upstream.
            if concurrency
                .get("cancel-in-progress")
                .and_then(Value::as_bool)
                != Some(false)
            {
                conflicts.push(format!("{name}{where_}"));
            }
        }
    }

    assert!(
        conflicts.is_empty(),
        "`queue: max` needs `cancel-in-progress: false`, not an expression that can read true: {conflicts:?}"
    );
}

// What a group expression can evaluate to, down to the part no branch varies:
// `heavy-linux-{0}` and `heavy-linux-${{ github.repository }}` name one queue.
fn group_prefix(group: &str) -> String {
    group
        .split(['{', '$'])
        .next()
        .unwrap_or(group)
        .trim()
        .to_owned()
}

#[test]
fn a_called_workflow_never_waits_on_the_group_its_caller_holds() {
    let mut deadlocks = Vec::new();
    for name in workflow_file_names() {
        let workflow = github_workflow(&name);
        let held = concurrency_prefixes(workflow.as_mapping().expect("workflow is a mapping"));
        for (job_name, job) in workflow_jobs(&workflow) {
            let job = job.as_mapping().expect("workflow job is a mapping");
            let Some(called) = job
                .get("uses")
                .and_then(Value::as_str)
                .and_then(|uses| uses.strip_prefix("./.github/workflows/"))
            else {
                continue;
            };
            let mut caller = held.clone();
            caller.extend(concurrency_prefixes(job));
            let callee = concurrency_prefixes(
                github_workflow(called)
                    .as_mapping()
                    .expect("called workflow is a mapping"),
            );
            let shared: Vec<&String> = caller.intersection(&callee).collect();
            if !shared.is_empty() {
                let job_name = job_name.as_str().expect("workflow job name is a string");
                deadlocks.push(format!(
                    "{name} job `{job_name}` calls {called} on {shared:?}"
                ));
            }
        }
    }

    assert!(
        deadlocks.is_empty(),
        "a called workflow cannot start while its caller holds the same concurrency group: {deadlocks:?}"
    );
}

#[test]
fn the_heavy_lanes_queue_together_and_ordinary_ci_queues_per_branch() {
    // One machine serves every lane, and the queue that protects it must not
    // also stall the pushes. Measured on the Linux host before this split, when
    // every Linux workflow named one group per repository: a nightly assessment
    // held the only slot with three jobs while two pushes waited fourteen
    // minutes with fifteen of eighteen runner slots and two thirds of the CPU
    // idle. Coverage, the extra suites, Miri, mutation, and assessment ask for
    // the whole fleet, so they take turns; a push does not wait behind them.
    // `network.yml` holds the group itself. `dispatch.yml` holds it on the jobs
    // that call the fan-out for the heavy roles, because a lane cannot: one
    // `run.yml` serves every role, and a group declared there would serialise
    // the light lanes with the heavy ones.
    let network = github_workflow("network.yml");
    let concurrency = workflow_concurrency(&network);
    assert_eq!(
        mapping_field(concurrency, "group").as_str(),
        Some(HEAVY_LINUX_GROUP)
    );
    assert_eq!(mapping_field(concurrency, "queue").as_str(), Some("max"));

    let dispatch = github_workflow("dispatch.yml");
    let jobs = workflow_jobs(&dispatch);
    for role in ["deep", "quality"] {
        let job = workflow_job(jobs, role);
        let group = mapping_field(job, "concurrency")
            .as_mapping()
            .unwrap_or_else(|| panic!("the {role} call names a concurrency group"));
        assert_eq!(
            mapping_field(group, "group").as_str(),
            Some(HEAVY_LINUX_GROUP),
            "{role}"
        );
        assert_eq!(
            mapping_field(group, "queue").as_str(),
            Some("max"),
            "{role}"
        );
    }

    // Every lane that says it wants the whole fleet must be scheduled by one of
    // those two calls; a lane declaring the queue under a role nobody holds the
    // group for would run beside the pushes it is meant to take turns with.
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask has a workspace root");
    let config: toml::Value = toml::from_str(
        &fs::read_to_string(root.join(".config/xtask.toml")).expect("xtask config is readable"),
    )
    .expect("xtask config is valid TOML");
    for (name, lane) in config["ext"]["ci"]["lanes"]
        .as_table()
        .expect("the catalog is a table")
    {
        if lane.get("queue").and_then(toml::Value::as_str) != Some("heavy-linux") {
            continue;
        }
        let role = lane["role"].as_str().expect("a lane names a role");
        assert!(
            matches!(role, "deep" | "mutants" | "quality"),
            "lane `{name}` queues with the heavy lanes under role `{role}`, \
             which holds no group"
        );
    }

    let ordinary = github_workflow("ci.yml");
    let group = mapping_field(workflow_concurrency(&ordinary), "group")
        .as_str()
        .expect("the CI group is a string")
        .to_owned();
    assert!(group.contains("github.ref"), "{group}");
    assert_ne!(group_prefix(&group), group_prefix(HEAVY_LINUX_GROUP));
}

// The three lanes are separate so a red one names which playback path broke,
// and each pins the standalone backend: one gate runs one backend, and a second
// axis doubles the runner's bill. The instrumented backend is no longer offered
// from a run dialog - the catalog carries no per-run knobs - and an
// investigation sets `KITHARA_RTSAN_BACKEND` for itself.
#[test]
fn standalone_rtsan_is_fail_closed_before_expanding_every_lane() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask has a workspace root");
    let config: toml::Value = toml::from_str(
        &fs::read_to_string(root.join(".config/xtask.toml")).expect("xtask config is readable"),
    )
    .expect("xtask config is valid TOML");
    let lanes = config["ext"]["ci"]["lanes"]
        .as_table()
        .expect("the catalog is a table");

    let mut artifacts = BTreeSet::new();
    for (name, recipe) in [
        ("deep-rtsan-fast", "rtsan"),
        ("deep-rtsan-file", "rtsan-file"),
        ("deep-rtsan-hls", "rtsan-hls"),
    ] {
        let lane = lanes[name].as_table().unwrap_or_else(|| panic!("{name}"));
        assert_eq!(lane["os"].as_str(), Some("linux"), "{name}");
        let steps = lane["steps"].as_array().expect("a lane has steps");
        assert_eq!(
            steps.len(),
            1,
            "{name} runs one backend, not a matrix of them"
        );
        let step = steps[0].as_table().expect("a step is a table");
        let args: Vec<&str> = step["args"]
            .as_array()
            .expect("a step has args")
            .iter()
            .filter_map(toml::Value::as_str)
            .collect();
        assert_eq!(args, ["test", recipe], "{name}");
        assert_eq!(
            step["env"]["KITHARA_RTSAN_BACKEND"].as_str(),
            Some("standalone"),
            "{name}"
        );

        // A shared artifact name would have the three lanes overwrite each
        // other's evidence, and the report would describe one of them.
        let artifact = lane["artifact"]
            .as_table()
            .expect("a lane names its artifact");
        assert_eq!(
            artifact["path"].as_str(),
            Some("target/nextest/rtsan/junit.xml"),
            "{name}"
        );
        assert_eq!(artifact["when"].as_str(), Some("always"), "{name}");
        assert!(
            artifacts.insert(artifact["name"].as_str().expect("an artifact is named")),
            "{name} shares its artifact name with another RTSan lane"
        );
    }
}

#[test]
fn stress_workflow_is_a_thin_fork_adapter() {
    let text = github_workflow_text("stress.yml");
    for forbidden in [
        "python3",
        "sample_pressure",
        "write_manifest",
        "cargo run",
        "stress-run",
        "just tooling",
        "--test-threads",
        "--flash",
        "--diagnostics",
        "--no-block",
        "--dump-thread-backtrace",
        "--job-timeout-minutes",
        "--expected-filter",
        "--expected-count",
        "--expected-mode",
        "--expected-test-threads",
        "--expected-flash",
        "--expected-no-block",
        "--expected-dump-thread-backtrace",
        "--expected-job-timeout-minutes",
    ] {
        assert!(
            !text.contains(forbidden),
            "stress workflow contains portable implementation detail {forbidden:?}"
        );
    }

    let workflow: Value = serde_yaml_ng::from_str(&text).expect("stress workflow is valid YAML");
    assert_no_key(&workflow, "continue-on-error");
    let concurrency = workflow_concurrency(&workflow);
    // The run queues in a group of its own: it runs on a dedicated
    // runner, and sharing fork CI's group meant a run dispatched behind
    // an already-queued lane was cancelled outright rather than queued.
    assert_eq!(
        mapping_field(concurrency, "group").as_str(),
        Some("stress-${{ github.repository }}")
    );
    assert_eq!(
        mapping_field(concurrency, "cancel-in-progress").as_bool(),
        Some(false)
    );
    assert_eq!(mapping_field(concurrency, "queue").as_str(), Some("max"));

    let root = workflow.as_mapping().expect("workflow is a mapping");
    let permissions = mapping_field(root, "permissions")
        .as_mapping()
        .expect("permissions are a mapping");
    assert_eq!(permissions.len(), 1);
    assert_eq!(
        mapping_field(permissions, "contents").as_str(),
        Some("read")
    );

    let jobs = workflow_jobs(&workflow);
    assert_eq!(
        workflow_job_names(jobs),
        BTreeSet::from([
            "authorize".to_owned(),
            "execute".to_owned(),
            "report".to_owned(),
        ])
    );
}

#[test]
fn stress_workflow_inputs_own_no_defaults() {
    let workflow = github_workflow("stress.yml");
    let root = workflow.as_mapping().expect("workflow is a mapping");
    let triggers = mapping_field(root, "on")
        .as_mapping()
        .expect("workflow triggers are a mapping");
    for trigger in ["workflow_call", "workflow_dispatch"] {
        let inputs = mapping_field(
            mapping_field(triggers, trigger)
                .as_mapping()
                .unwrap_or_else(|| panic!("{trigger} is a mapping")),
            "inputs",
        )
        .as_mapping()
        .expect("workflow inputs are a mapping");
        assert_eq!(
            inputs
                .keys()
                .map(|name| name.as_str().expect("input name is a string"))
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["count", "filter", "lane", "mode", "revision"])
        );
        for name in ["count", "filter", "lane", "mode", "revision"] {
            let input = mapping_field(inputs, name)
                .as_mapping()
                .unwrap_or_else(|| panic!("{trigger} input `{name}` is a mapping"));
            assert!(
                !input.contains_key("default"),
                "{trigger} input `{name}` must not own a workflow-local default"
            );
        }
    }
}

#[test]
fn stress_authorization_admits_only_the_owner_on_a_fork() {
    let workflow = github_workflow("stress.yml");
    let jobs = workflow_jobs(&workflow);
    let authorize = workflow_job(jobs, "authorize");
    assert_eq!(
        mapping_field(authorize, "runs-on").as_str(),
        Some("ubuntu-latest")
    );
    assert!(!authorize.contains_key("if"));
    let authorization = first_step(authorize);
    assert_eq!(mapping_field(authorization, "shell").as_str(), Some("bash"));
    let authorization_env = mapping_field(authorization, "env")
        .as_mapping()
        .expect("stress authorization environment is a mapping");
    assert_eq!(authorization_env.len(), 9);
    for (name, expected) in [
        ("ACTOR", "${{ github.actor }}"),
        ("COUNT", "${{ inputs.count || vars.KITHARA_STRESS_COUNT }}"),
        ("ENABLED", "${{ vars.KITHARA_STRESS_ENABLED }}"),
        ("IS_FORK", "${{ github.event.repository.fork }}"),
        ("MAX_COUNT", "${{ vars.KITHARA_STRESS_MAX_COUNT }}"),
        ("OWNER", "${{ github.repository_owner }}"),
        ("REVISION", "${{ inputs.revision || github.sha }}"),
        ("RUNNER_LABELS", "${{ vars.KITHARA_STRESS_RUNNER_LABELS }}"),
        ("TRIGGERING_ACTOR", "${{ github.triggering_actor }}"),
    ] {
        assert_eq!(
            mapping_field(authorization_env, name).as_str(),
            Some(expected)
        );
    }
    let authorization_script = mapping_field(authorization, "run")
        .as_str()
        .expect("authorization is a script");
    for contract in [
        "[[ \"$ENABLED\" == true ]]",
        "[[ \"$IS_FORK\" == true ]]",
        "[[ \"$ACTOR\" == \"$OWNER\" ]]",
        "[[ \"$TRIGGERING_ACTOR\" == \"$OWNER\" ]]",
        "[[ \"$REVISION\" =~ ^[0-9a-fA-F]{40}$ ]]",
        "[[ \"$COUNT\" =~ ^[1-9][0-9]*$ ]]",
        "[[ \"$MAX_COUNT\" =~ ^[1-9][0-9]*$ ]]",
        "10#$COUNT <= 10#$MAX_COUNT",
        "\"self-hosted\", \"linux\", \"x64\", \"kithara-stress\"",
        "<<< \"$RUNNER_LABELS\"",
    ] {
        assert!(
            authorization_script.contains(contract),
            "stress authorization omits {contract:?}"
        );
    }
}

#[test]
fn stress_execute_runs_the_portable_command_on_the_subject() {
    let workflow = github_workflow("stress.yml");
    let jobs = workflow_jobs(&workflow);
    let execute = workflow_job(jobs, "execute");
    assert_eq!(job_needs(execute), BTreeSet::from(["authorize".to_owned()]));
    let execute_guard = mapping_field(execute, "if")
        .as_str()
        .expect("execute guard is a string");
    for contract in [
        "github.event.repository.fork == true",
        "github.actor == github.repository_owner",
        "github.triggering_actor == github.repository_owner",
        "vars.KITHARA_STRESS_ENABLED == 'true'",
        "contains(fromJSON(vars.KITHARA_STRESS_RUNNER_LABELS), 'self-hosted')",
        "contains(fromJSON(vars.KITHARA_STRESS_RUNNER_LABELS), 'linux')",
        "contains(fromJSON(vars.KITHARA_STRESS_RUNNER_LABELS), 'x64')",
        "contains(fromJSON(vars.KITHARA_STRESS_RUNNER_LABELS), 'kithara-stress')",
    ] {
        assert!(
            execute_guard.contains(contract),
            "execute rerun guard omits {contract:?}"
        );
    }
    assert_eq!(
        mapping_field(execute, "runs-on").as_str(),
        Some("${{ fromJSON(vars.KITHARA_STRESS_RUNNER_LABELS) }}")
    );
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask has a workspace root");
    let config: toml::Value = toml::from_str(
        &fs::read_to_string(root.join(".config/xtask.toml")).expect("xtask config is readable"),
    )
    .expect("xtask config is valid TOML");
    let configured_timeout = config["stress"]["workflow_job_timeout_minutes"]
        .as_integer()
        .and_then(|value| u64::try_from(value).ok())
        .expect("stress job timeout is configured");
    assert_eq!(
        mapping_field(execute, "timeout-minutes").as_u64(),
        Some(configured_timeout)
    );
    let strategy = mapping_field(execute, "strategy")
        .as_mapping()
        .expect("stress must shard independent checkouts");
    assert_eq!(
        mapping_field(strategy, "fail-fast").as_bool(),
        Some(false),
        "one failed shard must not cancel the other repetitions"
    );
    assert_eq!(mapping_field(strategy, "max-parallel").as_u64(), Some(3));
    let matrix = mapping_field(strategy, "matrix")
        .as_mapping()
        .expect("stress matrix is a mapping");
    assert_eq!(
        mapping_field(matrix, "shard"),
        &Value::Sequence((0_u64..3).map(Value::from).collect())
    );
    assert_eq!(
        job_step_names(execute),
        BTreeSet::from([
            "Checkout controller".to_owned(),
            "Checkout subject".to_owned(),
            "Execute the stress run".to_owned(),
            "Upload the raw stress evidence".to_owned(),
        ])
    );

    assert_checkout(
        named_step(execute, "Checkout controller"),
        "controller",
        "${{ job.workflow_repository }}",
        "${{ job.workflow_sha }}",
    );
    assert_checkout(
        named_step(execute, "Checkout subject"),
        "subject",
        "${{ github.repository }}",
        "${{ inputs.revision || github.sha }}",
    );

    assert!(
        step_position(execute, "Checkout subject")
            < step_position(execute, "Execute the stress run")
    );

    let run = named_step(execute, "Execute the stress run");
    assert_eq!(
        mapping_field(run, "working-directory").as_str(),
        Some("controller")
    );
    let stress_env = mapping_field(run, "env")
        .as_mapping()
        .expect("run environment is a mapping");
    assert_eq!(stress_env.len(), 7);
    for (name, expected) in [
        ("CONTROLLER_SHA", "${{ job.workflow_sha }}"),
        ("COUNT", "${{ inputs.count || vars.KITHARA_STRESS_COUNT }}"),
        ("FILTER", "${{ inputs.filter }}"),
        ("LANE", "${{ inputs.lane }}"),
        ("MODE", "${{ inputs.mode }}"),
        ("SUBJECT_SHA", "${{ inputs.revision || github.sha }}"),
        ("SHARD_INDEX", "${{ matrix.shard }}"),
    ] {
        assert_eq!(mapping_field(stress_env, name).as_str(), Some(expected));
    }
    let execute_script = mapping_field(run, "run")
        .as_str()
        .expect("run command is a script")
        .trim();
    assert_eq!(execute_script, STRESS_EXECUTE_COMMAND);
    assert_eq!(execute_script.matches("just ci stress").count(), 1);

    let raw_upload = named_step(execute, "Upload the raw stress evidence");
    assert_uploads(raw_upload, &[STRESS_RAW_DIR]);
    let raw_upload_inputs = mapping_field(raw_upload, "with")
        .as_mapping()
        .expect("raw upload inputs are a mapping");
    assert_eq!(
        mapping_field(raw_upload_inputs, "name").as_str(),
        Some("stress-raw-${{ github.run_id }}-${{ github.run_attempt }}-shard-${{ matrix.shard }}")
    );
    assert_eq!(
        mapping_field(raw_upload_inputs, "retention-days").as_u64(),
        Some(14)
    );
    assert_eq!(
        mapping_field(raw_upload_inputs, "if-no-files-found").as_str(),
        Some("error")
    );
}

#[test]
fn stress_report_verifies_and_publishes_the_raw_evidence() {
    let workflow = github_workflow("stress.yml");
    let jobs = workflow_jobs(&workflow);
    let report = workflow_job(jobs, "report");
    assert_eq!(job_needs(report), BTreeSet::from(["execute".to_owned()]));
    assert_eq!(
        mapping_field(report, "runs-on").as_str(),
        Some("ubuntu-latest")
    );
    assert_eq!(
        mapping_field(report, "if").as_str(),
        Some("${{ always() && needs.execute.result != 'skipped' }}")
    );
    // The renderer is built from the workspace on the hosted image that owns
    // the reporting boundary, so the job carries the system libraries that
    // build links and nothing else. It links ALSA because the renderer reaches
    // the feature-unification crate through `kithara-derive`; until that edge
    // is gone the headers are a build prerequisite, not portable work.
    assert_eq!(
        job_step_names(report),
        BTreeSet::from([
            "Add the stress summary".to_owned(),
            "Checkout controller".to_owned(),
            "Download shard 0 evidence".to_owned(),
            "Download shard 1 evidence".to_owned(),
            "Download shard 2 evidence".to_owned(),
            "Install just".to_owned(),
            "Install the audio headers the renderer links".to_owned(),
            "Upload the stress evidence".to_owned(),
            "Verify and render the stress evidence".to_owned(),
        ])
    );

    assert_checkout(
        named_step(report, "Checkout controller"),
        "controller",
        "${{ job.workflow_repository }}",
        "${{ job.workflow_sha }}",
    );
    let mut artifact_names = BTreeSet::new();
    let mut artifact_roots = BTreeSet::new();
    for shard in 0..3 {
        let download = named_step(report, &format!("Download shard {shard} evidence"));
        assert_always(download);
        assert_action_repository(download, "actions/download-artifact");
        let inputs = mapping_field(download, "with")
            .as_mapping()
            .expect("artifact download inputs are a mapping");
        let name = format!(
            "stress-raw-${{{{ github.run_id }}}}-${{{{ github.run_attempt }}}}-shard-{shard}"
        );
        let path = format!("raw/shard-{shard}");
        assert_eq!(mapping_field(inputs, "name").as_str(), Some(name.as_str()));
        assert_eq!(mapping_field(inputs, "path").as_str(), Some(path.as_str()));
        assert!(artifact_names.insert(name));
        assert!(artifact_roots.insert(path));
        assert!(
            inputs.get("merge-multiple").is_none(),
            "artifact roots must remain separate so duplicate units cannot overwrite evidence"
        );
    }

    let report_install = named_step(report, "Install just");
    assert_always(report_install);
    assert_action_repository(report_install, "taiki-e/install-action");
    let install_inputs = mapping_field(report_install, "with")
        .as_mapping()
        .expect("install action inputs are a mapping");
    assert_eq!(mapping_field(install_inputs, "tool").as_str(), Some("just"));

    let verifier = named_step(report, "Verify and render the stress evidence");
    assert_always(verifier);
    assert_eq!(
        mapping_field(verifier, "working-directory").as_str(),
        Some("controller")
    );
    let verifier_env = mapping_field(verifier, "env")
        .as_mapping()
        .expect("verifier environment is a mapping");
    assert_eq!(verifier_env.len(), 7);
    for (name, expected) in [
        ("CONTROLLER_SHA", "${{ job.workflow_sha }}"),
        ("COUNT", "${{ inputs.count || vars.KITHARA_STRESS_COUNT }}"),
        ("EXECUTE_RESULT", "${{ needs.execute.result }}"),
        ("FILTER", "${{ inputs.filter }}"),
        ("LANE", "${{ inputs.lane }}"),
        ("MODE", "${{ inputs.mode }}"),
        ("SUBJECT_SHA", "${{ inputs.revision || github.sha }}"),
    ] {
        assert_eq!(mapping_field(verifier_env, name).as_str(), Some(expected));
    }
    let report_script = mapping_field(verifier, "run")
        .as_str()
        .expect("report command is a script")
        .trim();
    assert_eq!(report_script, STRESS_REPORT_COMMAND);
    assert_eq!(report_script.matches("just ci stress-report").count(), 1);

    let summary = named_step(report, "Add the stress summary");
    assert_always(summary);
    let summary_script = mapping_field(summary, "run")
        .as_str()
        .expect("summary command is a string");
    assert!(summary_script.contains("[[ -s \"$GITHUB_WORKSPACE/target/stress-report.md\" ]]"));
    assert!(
        summary_script.contains(
            "cat \"$GITHUB_WORKSPACE/target/stress-report.md\" >> \"$GITHUB_STEP_SUMMARY\""
        )
    );

    let evidence_upload = named_step(report, "Upload the stress evidence");
    assert_uploads(evidence_upload, &["raw", "target/stress-report.md"]);
    assert_eq!(
        mapping_field(
            mapping_field(evidence_upload, "with")
                .as_mapping()
                .expect("evidence upload inputs are a mapping"),
            "retention-days",
        )
        .as_u64(),
        Some(14)
    );
}

#[test]
fn scheduled_stress_respects_the_repository_switch_and_runner_pool() {
    let workflow = github_workflow("dispatch.yml");
    let stress = workflow_job(workflow_jobs(&workflow), "stress");
    assert_eq!(
        mapping_field(stress, "uses").as_str(),
        Some("./.github/workflows/stress.yml")
    );
    let condition = mapping_field(stress, "if")
        .as_str()
        .expect("scheduled stress condition is a string");
    let [_, weekly] = dispatch_crons(&workflow)[..] else {
        panic!("one cron per cadence");
    };
    let cadence = dispatch_cadence(weekly);
    let nightly_only = format!(
        "({}) == 'nightly'",
        cadence.trim_start_matches("${{ ").trim_end_matches(" }}")
    );
    for contract in [
        "vars.KITHARA_STRESS_ENABLED == 'true'",
        "vars.KITHARA_STRESS_RUNNER_LABELS != ''",
        nightly_only.as_str(),
    ] {
        assert!(
            condition.contains(contract),
            "scheduled stress omits `{contract}`"
        );
    }
}

/// The crons `dispatch.yml` is scheduled by, in declaration order.
fn dispatch_crons(workflow: &Value) -> Vec<&str> {
    let triggers = mapping_field(workflow.as_mapping().expect("workflow is a mapping"), "on")
        .as_mapping()
        .expect("on is a mapping");
    mapping_field(triggers, "schedule")
        .as_sequence()
        .expect("schedule is a sequence")
        .iter()
        .map(|entry| {
            mapping_field(
                entry.as_mapping().expect("a schedule entry is a mapping"),
                "cron",
            )
            .as_str()
            .expect("a cron is a string")
        })
        .collect()
}

/// How every dispatched job resolves its cadence: the input when started by
/// hand, otherwise which of the two crons fired.
fn dispatch_cadence(weekly_cron: &str) -> String {
    format!(
        "${{{{ inputs.kind || (github.event.schedule == '{weekly_cron}' && 'weekly' || 'nightly') }}}}"
    )
}

/// Eleven crons fired the same workflow so that one job ran and ten skipped,
/// and the collector then reported on each nearly-empty run. What a night runs
/// is a property of the lanes, not of which cron fired, so there is one cron
/// per cadence and the cadence is an input every job reads the same way.
#[test]
fn the_dispatcher_has_one_cron_per_cadence() {
    let workflow = github_workflow("dispatch.yml");
    let root = workflow.as_mapping().expect("workflow is a mapping");
    let triggers = mapping_field(root, "on")
        .as_mapping()
        .expect("on is a mapping");

    let crons = dispatch_crons(&workflow);
    let [nightly, weekly] = crons.as_slice() else {
        panic!("one cron per cadence, a night and a week: {crons:?}");
    };
    assert!(
        nightly.ends_with(" * * *") && !weekly.ends_with(" * * *"),
        "the first cron fires every day, the second on one day of the week: {crons:?}"
    );
    // GitHub starts a schedule late when load peaks, and it peaks at the top
    // of every hour: a night declared for 01:00 started between 06:02 and
    // 07:11, inside the morning's pushes it was scheduled to stay out of.
    for cron in &crons {
        assert_ne!(
            cron.split_whitespace().next(),
            Some("0"),
            "cron `{cron}` fires at the top of the hour"
        );
    }

    // Started by hand, the cadence is chosen rather than inferred, and `only`
    // is how one lane runs on its own instead of a role's whole selection.
    let inputs = mapping_field(
        mapping_field(triggers, "workflow_dispatch")
            .as_mapping()
            .expect("workflow_dispatch is a mapping"),
        "inputs",
    )
    .as_mapping()
    .expect("dispatch inputs are a mapping");
    let kind = mapping_field(inputs, "kind")
        .as_mapping()
        .expect("kind is a mapping");
    assert_eq!(mapping_field(kind, "type").as_str(), Some("choice"));
    assert_eq!(
        mapping_field(kind, "options")
            .as_sequence()
            .expect("kind options are a sequence")
            .iter()
            .map(|option| option.as_str().expect("an option is a string"))
            .collect::<Vec<_>>(),
        ["nightly", "weekly"]
    );
    assert!(inputs.contains_key("only"), "one lane runs on its own");

    // Every role reaches the fleet through the one fan-out, and none of them
    // reads the weekly cron differently from the others: a cadence resolved
    // two ways is a night that half-runs.
    let jobs = workflow_jobs(&workflow);
    let cadence = dispatch_cadence(weekly);
    let cadence = cadence.as_str();
    for role in ["gate", "platforms", "deep", "quality"] {
        let job = workflow_job(jobs, role);
        assert_eq!(
            mapping_field(job, "uses").as_str(),
            Some("./.github/workflows/run.yml"),
            "role `{role}` runs through the fan-out"
        );
        let with = mapping_field(job, "with")
            .as_mapping()
            .expect("a role call passes inputs");
        assert_eq!(mapping_field(with, "role").as_str(), Some(role));
        assert_eq!(mapping_field(with, "kind").as_str(), Some(cadence));
        assert_eq!(
            mapping_field(with, "only").as_str(),
            Some("${{ inputs.only || '' }}")
        );
    }

    // The workflows a night runs beside the fan-out admit themselves by that
    // same cadence. Written as `inputs.kind || 'nightly'`, a scheduled run
    // carries no input and each of them called itself nightly whichever cron
    // fired: the Saturday run then drove the fan-out weekly and these four
    // nightly, and a week's run also spent the fleet's hours on the repeated
    // run that is declared nightly-only.
    let cadence_expression = cadence.trim_start_matches("${{ ").trim_end_matches(" }}");
    for name in ["network", "windows", "ui", "stress"] {
        let condition = mapping_field(workflow_job(jobs, name), "if")
            .as_str()
            .expect("a nightly-only workflow states the cadence it runs in");
        assert!(
            condition.contains(&format!("({cadence_expression}) == 'nightly'")),
            "job `{name}` resolves its cadence without reading the cron: {condition}"
        );
    }

    // A copy of the repository without the runners spends a scheduled run
    // doing nothing, rather than a machine's night doing nothing.
    for name in workflow_job_names(jobs) {
        let job = workflow_job(jobs, &name);
        let condition = mapping_field(job, "if")
            .as_str()
            .expect("every dispatched job names the pool it needs");
        assert!(
            condition.contains("RUNNER_LABELS != ''") || condition.contains("STRESS_ENABLED"),
            "job `{name}` starts without checking that a pool serves it"
        );
    }
}

/// The dispatcher had no group of its own, so nothing superseded a run: a
/// branch that dispatched its night on every push queued a whole night per
/// push, each waiting its turn on the fleet behind the last. A run started
/// again for the same branch, cadence and selection makes the one before it
/// obsolete; a different selection, or the other cadence, is another question
/// and keeps its run.
#[test]
fn a_newer_dispatch_of_one_selection_supersedes_the_older() {
    let workflow = github_workflow("dispatch.yml");
    let concurrency = workflow_concurrency(&workflow);
    let group = mapping_field(concurrency, "group")
        .as_str()
        .expect("the dispatch group is a string");
    let [_, weekly] = dispatch_crons(&workflow)[..] else {
        panic!("one cron per cadence");
    };
    let cadence = dispatch_cadence(weekly);
    for part in [
        "github.ref",
        cadence.trim_start_matches("${{ ").trim_end_matches(" }}"),
        "inputs.only",
        "inputs.narrow",
        "inputs.mutants",
    ] {
        assert!(
            group.contains(part),
            "the dispatch group omits `{part}`: {group}"
        );
    }
    assert_eq!(
        mapping_field(concurrency, "cancel-in-progress").as_bool(),
        Some(true)
    );
}

#[test]
fn stress_profile_records_failures_with_a_dump_aware_outer_backstop() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask has a workspace root");
    let nextest: toml::Value = toml::from_str(
        &fs::read_to_string(root.join(".config/nextest.toml")).expect("nextest config is readable"),
    )
    .expect("nextest config is valid TOML");
    let stress = nextest["profile"]["stress"]
        .as_table()
        .expect("stress profile is a table");

    assert!(!stress.contains_key("retries"));
    assert_eq!(stress["fail-fast"].as_bool(), Some(false));
    let timeout = stress["slow-timeout"]
        .as_table()
        .expect("stress timeout is a table");
    let period = timeout["period"]
        .as_str()
        .and_then(|period| period.strip_suffix('s'))
        .and_then(|seconds| seconds.parse::<u64>().ok())
        .expect("stress timeout period is seconds");
    let terminate_after = timeout["terminate-after"]
        .as_integer()
        .and_then(|count| u64::try_from(count).ok())
        .expect("stress termination count is positive");
    let outer = period * terminate_after;
    let prekill = configured_stress_prekill_secs(root);
    assert!(outer >= prekill + 30);

    let junit = stress["junit"].as_table().expect("stress JUnit is a table");
    assert_eq!(junit["path"].as_str(), Some("junit.xml"));
    assert_eq!(junit["store-failure-output"].as_bool(), Some(true));
}

#[test]
fn stress_backstop_covers_every_kithara_test_timeout() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask has a workspace root");
    let mut observed_max = 0_u64;
    let prekill = configured_stress_prekill_secs(root);
    let mut excessive = Vec::new();
    let mut unsupported = Vec::new();
    let mut observed = 0_usize;
    for tree in ["crates", "tests"] {
        let pattern = root.join(tree).join("**/*.rs");
        let pattern = pattern.to_str().expect("source glob path is UTF-8");
        for path in glob::glob(pattern).expect("source glob is valid") {
            let path = path.expect("source path is readable");
            let source = fs::read_to_string(&path).expect("Rust source is readable");
            let file = match syn::parse_file(&source) {
                Ok(file) => file,
                Err(error) => {
                    unsupported.push(format!(
                        "{}: cannot parse Rust source: {error}",
                        path.display()
                    ));
                    continue;
                }
            };
            let mut constants = TimeoutConstants::default();
            constants.visit_file(&file);
            let tokens = match source.parse::<TokenStream>() {
                Ok(tokens) => tokens,
                Err(error) => {
                    unsupported.push(format!(
                        "{}: cannot tokenize Rust source: {error}",
                        path.display()
                    ));
                    continue;
                }
            };
            let mut expressions = Vec::new();
            collect_timeout_attributes(&tokens, &path, &mut expressions, &mut unsupported);
            for expression in expressions {
                observed = observed.saturating_add(1);
                let Some(seconds) = timeout_seconds(&expression, &constants.values) else {
                    unsupported.push(format!(
                        "{}: unsupported kithara test timeout expression",
                        path.display()
                    ));
                    continue;
                };
                observed_max = observed_max.max(seconds);
                if seconds.saturating_add(30) > prekill {
                    excessive.push(format!("{}: {seconds}s", path.display()));
                }
            }
        }
    }

    assert!(observed > 0, "no kithara test timeout attributes found");
    assert!(
        unsupported.is_empty(),
        "kithara test timeouts escaped the closed source audit: {unsupported:?}"
    );
    assert!(
        excessive.is_empty(),
        "declared test timeouts leave less than 30s before the configured pre-kill snapshot: {excessive:?}"
    );
    assert!(prekill >= observed_max + 30);
}

#[derive(Default)]
struct TimeoutConstants {
    values: BTreeMap<String, u64>,
}

impl<'ast> Visit<'ast> for TimeoutConstants {
    fn visit_item_const(&mut self, item: &'ast ItemConst) {
        if let Some(value) = integer_seconds(&item.expr, &self.values) {
            self.values
                .entry(item.ident.to_string())
                .and_modify(|current| *current = (*current).max(value))
                .or_insert(value);
        }
    }
}

fn collect_timeout_attributes(
    stream: &TokenStream,
    path: &Path,
    expressions: &mut Vec<Expr>,
    problems: &mut Vec<String>,
) {
    let tokens = stream.clone().into_iter().collect::<Vec<_>>();
    let mut index = 0;
    while index < tokens.len() {
        if matches!(&tokens[index], TokenTree::Punct(punct) if punct.as_char() == '#') {
            let mut group_index = index + 1;
            if matches!(tokens.get(group_index), Some(TokenTree::Punct(punct)) if punct.as_char() == '!')
            {
                group_index += 1;
            }
            if let Some(TokenTree::Group(group)) = tokens.get(group_index)
                && group.delimiter() == Delimiter::Bracket
            {
                collect_timeout_meta(&group.stream(), path, expressions, problems);
                collect_timeout_attributes(&group.stream(), path, expressions, problems);
                index = group_index + 1;
                continue;
            }
        }
        if let TokenTree::Group(group) = &tokens[index] {
            collect_timeout_attributes(&group.stream(), path, expressions, problems);
        }
        index += 1;
    }
}

fn collect_timeout_meta(
    tokens: &TokenStream,
    path: &Path,
    expressions: &mut Vec<Expr>,
    problems: &mut Vec<String>,
) {
    let meta = match syn::parse2::<Meta>(tokens.clone()) {
        Ok(meta) => meta,
        Err(error) => {
            if starts_with_ident(tokens, "kithara") || starts_with_ident(tokens, "cfg_attr") {
                problems.push(format!(
                    "{}: cannot parse timeout-bearing attribute: {error}",
                    path.display()
                ));
            }
            return;
        }
    };
    collect_parsed_timeout_meta(&meta, path, expressions, problems);
}

fn collect_parsed_timeout_meta(
    meta: &Meta,
    path: &Path,
    expressions: &mut Vec<Expr>,
    problems: &mut Vec<String>,
) {
    if is_kithara_test(meta.path()) {
        let Meta::List(list) = meta else {
            return;
        };
        let arguments =
            match Punctuated::<Expr, Token![,]>::parse_terminated.parse2(list.tokens.clone()) {
                Ok(arguments) => arguments,
                Err(error) => {
                    problems.push(format!(
                        "{}: cannot parse kithara test attribute: {error}",
                        path.display()
                    ));
                    return;
                }
            };
        for argument in arguments {
            let Expr::Call(call) = argument else {
                continue;
            };
            if call_name(&call).as_deref() != Some("timeout") {
                continue;
            }
            if call.args.len() != 1 {
                problems.push(format!(
                    "{}: timeout must contain exactly one expression",
                    path.display()
                ));
                continue;
            }
            expressions.extend(call.args.first().cloned());
        }
        return;
    }

    if meta.path().is_ident("cfg_attr")
        && let Meta::List(list) = meta
    {
        let nested =
            match Punctuated::<Meta, Token![,]>::parse_terminated.parse2(list.tokens.clone()) {
                Ok(nested) => nested,
                Err(error) => {
                    problems.push(format!(
                        "{}: cannot parse cfg_attr while auditing timeouts: {error}",
                        path.display()
                    ));
                    return;
                }
            };
        for nested_meta in nested.iter().skip(1) {
            collect_parsed_timeout_meta(nested_meta, path, expressions, problems);
        }
    }
}

fn starts_with_ident(tokens: &TokenStream, expected: &str) -> bool {
    matches!(tokens.clone().into_iter().next(), Some(TokenTree::Ident(ident)) if ident == expected)
}

fn is_kithara_test(path: &syn::Path) -> bool {
    let mut segments = path.segments.iter();
    segments
        .next()
        .is_some_and(|segment| segment.ident == "kithara")
        && segments
            .next()
            .is_some_and(|segment| segment.ident == "test")
        && segments.next().is_none()
}

#[test]
fn timeout_source_audit_reads_macro_rules_and_cfg_attr_tokens() {
    let source = r##"
        const TEXT: &str = "#[kithara::test(timeout(Duration::from_secs(999)))]";
        macro_rules! generated_test {
            () => {
                #[cfg_attr(unix, kithara::test(tokio, timeout(Duration::from_secs(5))))]
                async fn generated() {}
            };
        }
    "##;
    let tokens = source.parse::<TokenStream>().expect("fixture tokenizes");
    let mut expressions = Vec::new();
    let mut problems = Vec::new();
    collect_timeout_attributes(
        &tokens,
        Path::new("macro-fixture.rs"),
        &mut expressions,
        &mut problems,
    );

    assert!(
        problems.is_empty(),
        "unexpected audit problems: {problems:?}"
    );
    assert_eq!(expressions.len(), 1);
    assert_eq!(timeout_seconds(&expressions[0], &BTreeMap::new()), Some(5));
}

fn timeout_seconds(expression: &Expr, constants: &BTreeMap<String, u64>) -> Option<u64> {
    match expression {
        Expr::Call(call)
            if call_name(call).as_deref() == Some("from_secs") && call.args.len() == 1 =>
        {
            let segments = call_path(call)?;
            (segments.iter().rev().nth(1).map(String::as_str) == Some("Duration"))
                .then(|| call.args.first())
                .flatten()
                .and_then(|argument| integer_seconds(argument, constants))
        }
        Expr::Call(call)
            if call_name(call).as_deref() == Some("browser_timeout") && call.args.len() == 2 =>
        {
            call.args
                .iter()
                .map(|argument| integer_seconds(argument, constants))
                .collect::<Option<Vec<_>>>()
                .and_then(|values| values.into_iter().max())
        }
        Expr::If(branch) => {
            let then_value = block_value(&branch.then_branch)
                .and_then(|value| timeout_seconds(value, constants))?;
            let else_value = branch
                .else_branch
                .as_ref()
                .and_then(|(_, value)| timeout_seconds(value, constants))?;
            Some(then_value.max(else_value))
        }
        Expr::Block(block) => {
            block_value(&block.block).and_then(|value| timeout_seconds(value, constants))
        }
        Expr::Group(group) => timeout_seconds(&group.expr, constants),
        Expr::Paren(paren) => timeout_seconds(&paren.expr, constants),
        _ => None,
    }
}

fn integer_seconds(expression: &Expr, constants: &BTreeMap<String, u64>) -> Option<u64> {
    match expression {
        Expr::Lit(literal) => match &literal.lit {
            syn::Lit::Int(value) => value.base10_parse().ok(),
            _ => None,
        },
        Expr::Path(path) if path.qself.is_none() && path.path.segments.len() == 1 => constants
            .get(&path.path.segments.first()?.ident.to_string())
            .copied(),
        Expr::Binary(binary) if matches!(binary.op, BinOp::Add(_)) => {
            integer_seconds(&binary.left, constants)?
                .checked_add(integer_seconds(&binary.right, constants)?)
        }
        Expr::Group(group) => integer_seconds(&group.expr, constants),
        Expr::Paren(paren) => integer_seconds(&paren.expr, constants),
        _ => None,
    }
}

fn call_name(call: &syn::ExprCall) -> Option<String> {
    call_path(call).and_then(|segments| segments.last().cloned())
}

fn call_path(call: &syn::ExprCall) -> Option<Vec<String>> {
    let Expr::Path(path) = call.func.as_ref() else {
        return None;
    };
    Some(
        path.path
            .segments
            .iter()
            .map(|segment| segment.ident.to_string())
            .collect(),
    )
}

fn block_value(block: &syn::Block) -> Option<&Expr> {
    match block.stmts.last()? {
        Stmt::Expr(expression, None) => Some(expression),
        _ => None,
    }
}

#[test]
fn the_dispatch_collector_is_read_only_and_does_not_mirror_the_source_verdict() {
    let workflow = github_workflow("report.yml");
    assert_no_key(&workflow, "continue-on-error");
    let root = workflow.as_mapping().expect("workflow is a mapping");
    let permissions = mapping_field(root, "permissions")
        .as_mapping()
        .expect("permissions are a mapping");
    assert_eq!(
        permissions
            .keys()
            .map(|name| name.as_str().expect("permission name is a string"))
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["actions", "contents"])
    );
    for permission in ["actions", "contents"] {
        assert_eq!(
            mapping_field(permissions, permission).as_str(),
            Some("read")
        );
    }

    let job = workflow_job(workflow_jobs(&workflow), "report");
    assert_eq!(
        mapping_field(job, "runs-on").as_str(),
        Some("ubuntu-latest")
    );
    // The collector shows the source verdict, so it runs for every conclusion
    // that is one. A skipped source has none, and reporting on it turned into a
    // red check about a run that never happened.
    assert_eq!(
        mapping_field(job, "if").as_str(),
        Some("github.event.workflow_run.conclusion != 'skipped'")
    );

    let collect = named_step(job, "Collect the source run jobs");
    let env = mapping_field(collect, "env")
        .as_mapping()
        .expect("the collector has an environment");
    assert_eq!(
        mapping_field(env, "RUN_ATTEMPT").as_str(),
        Some("${{ github.event.workflow_run.run_attempt }}")
    );
    let script = mapping_field(collect, "run")
        .as_str()
        .expect("dispatch collector is a script");
    for contract in [
        "gh api \"repos/$REPOSITORY/actions/runs/$RUN_ID/jobs\"",
        "--name quality-report-${RUN_ID}-${RUN_ATTEMPT}",
        "quality/consolidated-quality-report.md",
        "target/dispatch-report.md",
        "$GITHUB_STEP_SUMMARY",
    ] {
        assert!(
            script.contains(contract),
            "dispatch report omits `{contract}`"
        );
    }
    assert!(!script.contains("exit 1"));

    assert_uploads(
        named_step(job, "Upload the dispatch report"),
        &["target/dispatch-report.md"],
    );

    let text = github_workflow_text("report.yml");
    for forbidden in ["gh issue", "issues: write"] {
        assert!(
            !text.contains(forbidden),
            "dispatch collector contains forbidden `{forbidden}`"
        );
    }
}

// The image owns every pinned tool, browsers included. A run-time install puts
// a third-party download on the critical path of a job, which this repository
// has measured failing four runs out of six. See `docker/ci.Dockerfile`.
#[test]
fn a_browser_comes_from_the_image_and_is_never_fetched_by_a_job() {
    let mut installs = Vec::new();
    for name in workflow_file_names() {
        let workflow = github_workflow(&name);
        for (job_name, job) in workflow_jobs(&workflow) {
            let job_name = job_name.as_str().expect("workflow job name is a string");
            let Some(steps) = job.get("steps").and_then(Value::as_sequence) else {
                continue;
            };
            for step in steps {
                let uses = step.get("uses").and_then(Value::as_str).unwrap_or_default();
                let run = step.get("run").and_then(Value::as_str).unwrap_or_default();
                if uses.starts_with("browser-actions/") || run.contains("chrome-for-testing") {
                    installs.push(format!("{name} job `{job_name}`"));
                }
            }
        }
    }

    assert!(
        installs.is_empty(),
        "a browser is pinned in the CI image, never fetched by a job: {installs:?}"
    );
}

// `just ci run` is the GitLab entrypoint: it reads the host profile named by
// `KITHARA_CI_HOST_CONFIG`, which only the GitLab runner provisioning installs.
// A GitHub job that calls it dies before it reaches its own work, so a GitHub
// job calls the recipe the lane calls instead.
#[test]
fn a_github_job_never_calls_the_gitlab_only_lane_runner() {
    let mut callers = Vec::new();
    for name in workflow_file_names() {
        let workflow = github_workflow(&name);
        for (job_name, job) in workflow_jobs(&workflow) {
            let job_name = job_name.as_str().expect("workflow job name is a string");
            let Some(steps) = job.get("steps").and_then(Value::as_sequence) else {
                continue;
            };
            for step in steps {
                let run = step.get("run").and_then(Value::as_str).unwrap_or_default();
                if run.contains("just ci run") {
                    callers.push(format!("{name} job `{job_name}`"));
                }
            }
        }
    }

    assert!(
        callers.is_empty(),
        "`just ci run` needs the GitLab host profile: {callers:?}"
    );
}

// Mutation suites cost hours, and a push to the default branch asks for every
// lane, so neither a push nor a schedule starts them: only a dispatch that asks
// for all of them, or names the lanes it wants.
#[test]
fn mutation_suites_run_only_when_asked_for() {
    let ci = github_workflow("ci.yml");
    assert!(!workflow_job_names(workflow_jobs(&ci)).contains("mutants"));

    let dispatch = github_workflow("dispatch.yml");
    let condition = workflow_job(workflow_jobs(&dispatch), "mutants")
        .get("if")
        .and_then(Value::as_str)
        .expect("the mutation job is guarded");
    assert!(
        condition.contains("inputs.mutants || (inputs.only || '') != ''"),
        "{condition}"
    );
}

// `only` is how one subtask is run on its own, and the dispatcher hands the
// same list to every job it starts. The role fan-out answers it by rendering
// an empty selection for a lane it does not own; the jobs beside the fan-out
// have no selection to render, so they answer it in their own condition. A
// request for one lane that also started the Windows guest, the emulator and
// the stress campaign would be a request for one lane in name only.
/// A dispatch narrowed to a filterset narrows every lane it runs: each call
/// passes the filterset on, and the executor hands it to the lane through the
/// environment, never spliced into the script, since a filterset is shell
/// syntax.
#[test]
fn a_dispatched_narrowing_reaches_every_lane_it_runs() {
    let calls = local_workflow_calls();
    for (caller, callee, forwarded) in [
        ("dispatch.yml", "run.yml", "${{ inputs.narrow || '' }}"),
        ("run.yml", "lane.yml", "${{ inputs.narrow }}"),
    ] {
        let passed: Vec<_> = calls
            .iter()
            .filter(|(from, _, to, _)| from == caller && to == callee)
            .map(|(_, job, _, with)| (job.clone(), with.get("narrow").and_then(Value::as_str)))
            .collect();
        assert!(!passed.is_empty(), "{caller} calls {callee}");
        for (job, narrow) in passed {
            assert_eq!(
                narrow,
                Some(forwarded),
                "{caller} `{job}` drops the narrowing"
            );
        }
    }

    let workflow = github_workflow("lane.yml");
    let job = workflow_job(workflow_jobs(&workflow), "run");
    let step = named_step(job, "Run the lane");
    let env = mapping_field(step, "env")
        .as_mapping()
        .expect("the lane step has an environment");
    assert_eq!(
        mapping_field(env, "NARROW").as_str(),
        Some("${{ inputs.narrow }}")
    );
    let script = mapping_field(step, "run")
        .as_str()
        .expect("the lane step is a script");
    assert!(
        script.contains(r#"${NARROW:+--narrow "$NARROW"}"#),
        "{script}"
    );
}

#[test]
fn a_request_for_one_lane_starts_nothing_beside_it() {
    let workflow = github_workflow("dispatch.yml");
    let jobs = workflow_jobs(&workflow);
    let fan_out = ["gate", "platforms", "deep", "mutants", "quality"];

    for (name, job) in jobs {
        let name = name.as_str().expect("a dispatcher job name is a string");
        if fan_out.contains(&name) {
            continue;
        }
        let condition = job
            .get("if")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("`{name}` runs unconditionally"));
        assert!(
            condition.contains("inputs.only"),
            "`{name}` starts on a request for another lane: {condition}"
        );
    }

    // The UI suite is the one of them that is a declared lane, and this is
    // the only caller that can start it, so it answers to its own name rather
    // than standing aside for every request.
    let ui = workflow_job(jobs, "ui")
        .get("if")
        .and_then(Value::as_str)
        .expect("the UI job is guarded");
    assert!(
        ui.contains("contains(inputs.only, 'deep-ui')"),
        "the UI job answers to its lane's name: {ui}"
    );
}

fn ui_selection(jobs: &Mapping) -> (&str, &Mapping) {
    jobs.iter()
        .find_map(|(name, job)| {
            let job = job.as_mapping()?;
            job.get("outputs")?
                .as_mapping()?
                .contains_key("ui_required")
                .then(|| (name.as_str().expect("job name is a string"), job))
        })
        .expect("CI publishes automatic UI ownership selection")
}

#[test]
fn ui_ownership_selection_uses_the_fleet_checkout_and_build_environment() {
    let ci = github_workflow("ci.yml");
    let (_, selection) = ui_selection(workflow_jobs(&ci));
    let run = github_workflow("run.yml");
    let reference = workflow_job(workflow_jobs(&run), "select");
    assert_eq!(
        mapping_field(selection, "runs-on"),
        mapping_field(reference, "runs-on")
    );
    assert_eq!(first_step(selection), first_step(reference));
    assert_no_key(&Value::Mapping(selection.clone()), "fetch-depth");
    for variable in [
        "CARGO_TARGET_DIR",
        "CARGO_HOME",
        "KITHARA_CI_TARGET_ROOT",
        "KITHARA_CI_CACHE_ROOT",
        "KITHARA_XTASK_TARGET",
    ] {
        assert_no_key(&ci, variable);
    }
    let step = named_step(selection, "Select UI ownership");
    let step_id = mapping_field(step, "id")
        .as_str()
        .expect("selection step has an id");
    let outputs = mapping_field(selection, "outputs")
        .as_mapping()
        .expect("selection has outputs");
    assert_eq!(
        mapping_field(outputs, "ui_required").as_str(),
        Some(format!("${{{{ steps.{step_id}.outputs.ui_required }}}}").as_str())
    );
    assert_eq!(
        mapping_field(step, "run")
            .as_str()
            .expect("selection has a script")
            .trim(),
        "ui_required=$(just ci touched --lane ui)\necho \"ui_required=$ui_required\" >> \"$GITHUB_OUTPUT\""
    );
}

#[test]
fn ui_scheduling_consults_owned_path_selection() {
    let ci = github_workflow("ci.yml");
    let jobs = workflow_jobs(&ci);
    let (selection_name, _) = ui_selection(jobs);
    let ui = workflow_job(jobs, "ui");
    let condition = mapping_field(ui, "if")
        .as_str()
        .expect("UI scheduling is conditional");
    assert!(condition.contains("vars.KITHARA_GPU_RUNNER_LABELS != ''"));
    assert!(condition.contains(&format!(
        "needs.{selection_name}.outputs.ui_required == 'true'"
    )));
    assert!(job_needs(ui).contains(selection_name));
}

#[test]
fn required_aggregation_requires_an_automatically_selected_ui_verdict() {
    let ci = github_workflow("ci.yml");
    let jobs = workflow_jobs(&ci);
    let (selection_name, _) = ui_selection(jobs);
    let required = workflow_job(jobs, "required");
    assert!(job_needs(required).contains(selection_name));
    let step = first_step(required);
    let env = mapping_field(step, "env")
        .as_mapping()
        .expect("aggregate has an environment");
    assert_eq!(
        mapping_field(env, "TOUCHED_UI_REQUIRED").as_str(),
        Some(format!("${{{{ vars.KITHARA_GPU_RUNNER_LABELS != '' && needs.{selection_name}.outputs.ui_required == 'true' }}}}").as_str())
    );
    let verdict = mapping_field(step, "run")
        .as_str()
        .expect("aggregate has a verdict script");
    for (selected, lanes, result, accepted) in [
        ("true", "", "skipped", false),
        ("true", "", "failure", false),
        ("true", "", "cancelled", false),
        ("true", "", "success", true),
        ("false", "", "skipped", true),
        ("false", "", "failure", false),
        ("false", "all", "skipped", false),
        ("false", "deep-ui", "skipped", false),
        ("false", "deep-ui", "success", true),
    ] {
        let results = serde_json::json!({"ui": {"result": result}});
        let output = Command::new("bash")
            .args(["-c", verdict])
            .env("RESULTS", results.to_string())
            .env("REQUIRED_LANES", lanes)
            .env("TOUCHED_UI_REQUIRED", selected)
            .output()
            .expect("the real aggregate script executes");
        assert_eq!(
            output.status.success(),
            accepted,
            "selected={selected}, lanes={lanes}, result={result}"
        );
    }
}

#[test]
fn lane_jobs_do_not_wait_for_ui_ownership_selection() {
    let ci = github_workflow("ci.yml");
    let jobs = workflow_jobs(&ci);
    let (selection_name, selection) = ui_selection(jobs);
    assert_eq!(job_needs(selection), job_needs(workflow_job(jobs, "gate")));
    for (name, job) in jobs {
        let job = job.as_mapping().expect("job is a mapping");
        if job.get("uses").and_then(Value::as_str) == Some("./.github/workflows/run.yml") {
            assert!(
                !job_needs(job).contains(selection_name),
                "{name:?} waits for UI selection"
            );
        }
    }
    assert!(!job_needs(workflow_job(jobs, "gate")).contains(selection_name));
}

// A lane declared in the catalog and a workflow spelling out the same command
// are two places for one suite to change. The UI lane stays out of the role
// fan-out - it wants a runner pool of its own, and the fan-out schedules onto
// the single Linux pool - but staying out of the selection is not a licence to
// restate what it runs.
#[test]
fn the_ui_workflow_names_its_lane_instead_of_repeating_it() {
    let ci = github_workflow("ci.yml");
    let ui = workflow_job(workflow_jobs(&ci), "ui");
    assert_eq!(
        mapping_field(ui, "uses").as_str(),
        Some("./.github/workflows/ui.yml")
    );
    let condition = mapping_field(ui, "if")
        .as_str()
        .expect("the UI call is conditional");
    assert!(condition.contains("github.event.repository.default_branch"));
    assert!(condition.contains("inputs.required_lanes"));

    let text = github_workflow_text("ui.yml");
    assert!(
        text.contains("just ci lane deep-ui"),
        "the UI workflow runs the declared lane"
    );
    assert!(
        !text.contains("just test"),
        "the UI workflow carries no suite command of its own"
    );

    // Nothing selects the lane it names, on either provider, so this workflow
    // is the only caller that can start it. A kind appearing here would put a
    // GPU suite on a pool with no graphics device.
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask has a workspace root");
    let config: toml::Value = toml::from_str(
        &fs::read_to_string(root.join(".config/xtask.toml")).expect("xtask config is readable"),
    )
    .expect("xtask config is valid TOML");
    let lane = &config["ext"]["ci"]["lanes"]["deep-ui"];
    for field in ["kinds", "kinds_github"] {
        let kinds = lane[field]
            .as_array()
            .unwrap_or_else(|| panic!("deep-ui.{field} is an array"));
        assert!(
            kinds.is_empty(),
            "deep-ui.{field} schedules a GPU-less pool"
        );
    }
}

/// A job checks out the commit it runs, never every branch.
///
/// `actions/checkout` at depth zero fetches every branch and tag, and a fleet
/// runner keeps its checkout between jobs with garbage collection off. Every
/// lane defaulted to depth zero, so each runner's Git directory carried the
/// history of every branch the fork ever had, 12 to 20 GB a runner. A lane
/// that compares against an earlier revision reads the history of the commit
/// it checked out, which is the only history it compares against.
#[test]
fn a_checkout_never_fetches_every_branch() {
    for name in workflow_file_names() {
        let workflow = github_workflow(&name);
        for (job_name, job) in workflow_jobs(&workflow) {
            let Some(steps) = job.get("steps").and_then(Value::as_sequence) else {
                continue;
            };
            for step in steps.iter().filter_map(Value::as_mapping) {
                let is_checkout = step
                    .get("uses")
                    .and_then(Value::as_str)
                    .is_some_and(|uses| uses.starts_with("actions/checkout@"));
                let depth = step
                    .get("with")
                    .and_then(Value::as_mapping)
                    .and_then(|inputs| inputs.get("fetch-depth"));
                if let (true, Some(depth)) = (is_checkout, depth) {
                    assert!(
                        depth.as_u64().is_some_and(|depth| depth > 0),
                        "{name} job {job_name:?} checks out at depth {depth:?}, which can fetch \
                         every branch"
                    );
                }
            }
        }
    }
    let workflow = github_workflow("lane.yml");
    let history = named_step(
        workflow_job(workflow_jobs(&workflow), "run"),
        "Fetch the history of the checked-out commit",
    );
    assert_eq!(
        mapping_field(history, "if").as_str(),
        Some("${{ inputs.history }}"),
        "only a lane that reads history fetches it"
    );
    assert_eq!(
        mapping_field(history, "run").as_str().map(str::trim),
        Some(r#"git fetch --no-tags --quiet --depth=2147483647 origin "$(git rev-parse HEAD)""#),
        "a lane reads the history of the commit it checked out, not of the repository"
    );
}

/// A runner keeps its checkout for every later job, and checkout turns garbage
/// collection off in it, so each job's fetch left one more pack behind: up to
/// 670 of them on one runner. The lane collects that checkout's garbage on
/// Git's own thresholds once it has fetched everything it reads.
#[test]
fn a_kept_checkout_is_collected_on_gits_own_thresholds() {
    let workflow = github_workflow("lane.yml");
    let job = workflow_job(workflow_jobs(&workflow), "run");
    let collect = "Collect the checkout's garbage";
    assert_eq!(
        mapping_field(named_step(job, collect), "run")
            .as_str()
            .map(str::trim),
        Some("git -c gc.auto=6700 -c gc.autoDetach=false gc --auto --quiet")
    );
    assert!(
        step_position(job, "Fetch the history of the checked-out commit")
            < step_position(job, collect),
        "the garbage is collected after the last fetch"
    );
}

/// A step uploads the job-owned report directory, not the reusable build slot.
///
/// `ci lane` copies only this build's reports before releasing the slot and
/// announces `KITHARA_LANE_TIMINGS` only when it has a report. The upload runs
/// even after a failed lane, or is skipped when there is nothing to upload.
#[test]
fn a_step_that_collects_build_output_reads_the_build_directory() {
    let workflow = github_workflow("lane.yml");
    let steps = workflow
        .get("jobs")
        .and_then(|jobs| jobs.get("run"))
        .and_then(|job| job.get("steps"))
        .and_then(Value::as_sequence)
        .expect("the executor has steps");
    let timings = steps
        .iter()
        .find(|step| {
            step.get("name")
                .and_then(Value::as_str)
                .is_some_and(|name| name.contains("timing"))
        })
        .expect("the executor uploads a timing report");
    let path = timings
        .get("with")
        .and_then(|with| with.get("path"))
        .and_then(Value::as_str)
        .expect("the upload names a path");
    assert_eq!(
        path.trim(),
        "${{ env.KITHARA_LANE_TIMINGS }}",
        "the timing report must be read from the job-owned directory: {path}"
    );
    let condition = timings
        .get("if")
        .and_then(Value::as_str)
        .expect("the upload is conditional");
    assert_eq!(
        condition, "${{ always() && env.KITHARA_LANE_TIMINGS != '' }}",
        "failed lanes still upload their report without a lane list; a job whose build wrote no report has nothing to upload: {condition}"
    );
    assert_eq!(
        timings
            .get("with")
            .and_then(|with| with.get("if-no-files-found"))
            .and_then(Value::as_str),
        Some("error"),
        "an announced timing report must exist when the job uploads it"
    );
}

// The executor's whole job is to run a lane the catalog named. A workflow that
// can be handed an arbitrary command is a second place for a command to live.
#[test]
fn the_lane_executor_runs_a_named_lane_and_nothing_else() {
    let workflow = github_workflow("lane.yml");
    let on = mapping_field(workflow.as_mapping().expect("workflow is a mapping"), "on")
        .as_mapping()
        .expect("on is a mapping");
    let workflow_call = mapping_field(on, "workflow_call")
        .as_mapping()
        .expect("workflow_call is a mapping");
    let inputs = mapping_field(workflow_call, "inputs")
        .as_mapping()
        .expect("inputs are a mapping");
    assert!(
        inputs.contains_key("lane"),
        "the executor takes a lane name"
    );
    assert!(!inputs.contains_key("run"), "the executor takes no command");

    let text = github_workflow_text("lane.yml");
    assert!(
        text.contains("just ci lane \"${{ inputs.lane }}\" --kind \"${{ inputs.kind }}\""),
        "the executor runs the named lane"
    );

    // A caller reaches this through `uses:`, and GitHub renders the callee's
    // job name under the caller's own. Without this every lane of an
    // eighteen-way fan-out reads `run`, and which lane failed is legible only
    // through the API.
    let job = workflow_job(workflow_jobs(&workflow), "run");
    assert_eq!(
        mapping_field(job, "name").as_str(),
        Some("${{ inputs.lane }}"),
        "the executor's job carries the lane's name"
    );
    assert_eq!(
        mapping_field(job, "runs-on").as_str(),
        Some("${{ inputs.runner || fromJSON(vars.KITHARA_RUNNER_LABELS) }}"),
        "a lane runner label overrides only the shared pool selection"
    );
    let upload = named_step(job, "Upload the lane's report");
    let upload_inputs = mapping_field(upload, "with")
        .as_mapping()
        .expect("the lane upload has inputs");
    assert_eq!(
        mapping_field(upload_inputs, "name").as_str(),
        Some("${{ inputs.artifact-name }}-${{ github.run_id }}-${{ github.run_attempt }}")
    );
}

#[test]
fn external_git_fetches_use_scoped_checkout_token() {
    let workflow = github_workflow("lane.yml");
    let steps = workflow["jobs"]["run"]["steps"]
        .as_sequence()
        .expect("lane steps");
    let lane = steps
        .iter()
        .find(|step| step["name"].as_str() == Some("Run the lane"))
        .expect("lane command");
    assert_eq!(
        lane["env"]["GITHUB_FETCH_TOKEN"].as_str(),
        Some("${{ github.token }}")
    );
    let script = lane["run"].as_str().expect("lane script");
    assert!(
        script.contains(
            "GIT_CONFIG_KEY_0=http.https://github.com/gerasim13/bevy_seedling.extraheader"
        )
    );
    assert!(script.contains("GIT_CONFIG_VALUE_0=\"AUTHORIZATION: basic $auth\""));
    assert!(script.contains("GIT_CONFIG_COUNT=2"));
    assert!(
        script.contains("GIT_CONFIG_KEY_1=http.https://github.com/RustSec/advisory-db.extraheader")
    );
    assert!(script.contains("GIT_CONFIG_VALUE_1=\"AUTHORIZATION: basic $auth\""));
    assert!(
        !script.contains("url.https://"),
        "the token must not enter a git remote URL"
    );
}

// The other half of the same tree. Each calling job has to be named, because
// an unnamed one falls back to its id plus every matrix value it passed and
// the level above each lane reads `run (deep-rtsan-file, 120, 0,
// rtsan-junit-file, ...)` - the parameters, where a name belongs. The name may
// not come from the matrix: a job skipped by its `if:` never expands one, and
// GitHub prints the expression instead of a name, which is how a run tree came
// to end in a skipped `Gate / matrix.lane`. The lane's own name comes from the
// callee, which is rendered only when the job runs, so the caller's name has to
// stand on its own when it does not.
#[test]
fn the_fan_out_names_each_branch_without_reading_the_matrix() {
    let workflow = github_workflow("run.yml");
    let jobs = workflow_jobs(&workflow);
    for job in ["run", "dependent"] {
        let name = mapping_field(workflow_job(jobs, job), "name")
            .as_str()
            .unwrap_or_else(|| panic!("`{job}` is named"));
        assert!(
            !name.contains("matrix."),
            "`{job}` is named `{name}`, which a skipped job cannot resolve"
        );
    }
}

/// A caller and a called workflow agree on inputs across two files, and
/// nothing in the repository checked that they still do. A renamed input is
/// not a compile error and not a failing test: GitHub rejects the run at
/// validation time, the calling job never starts, and the aggregate reads a
/// job that never ran as a failure with no cause named in it.
fn called_workflow_inputs(name: &str) -> Option<Mapping> {
    let workflow = github_workflow(name);
    let on = workflow.as_mapping()?.get("on")?.as_mapping()?;
    // `workflow_call:` with nothing under it is a workflow that takes no
    // inputs, not a workflow that cannot be called.
    let Some(call) = on.get("workflow_call")?.as_mapping() else {
        return Some(Mapping::new());
    };
    call.get("inputs").map_or_else(
        || Some(Mapping::new()),
        |inputs| inputs.as_mapping().cloned(),
    )
}

fn local_workflow_calls() -> Vec<(String, String, String, Mapping)> {
    let mut calls = Vec::new();
    for name in workflow_file_names() {
        let workflow = github_workflow(&name);
        for (job_name, job) in workflow_jobs(&workflow) {
            let job_name = job_name.as_str().expect("workflow job name is a string");
            let Some(job) = job.as_mapping() else {
                continue;
            };
            let Some(uses) = job.get("uses").and_then(Value::as_str) else {
                continue;
            };
            let Some(called) = uses.strip_prefix("./.github/workflows/") else {
                continue;
            };
            let passed = job
                .get("with")
                .and_then(Value::as_mapping)
                .cloned()
                .unwrap_or_default();
            calls.push((name.clone(), job_name.to_owned(), called.to_owned(), passed));
        }
    }
    calls
}

#[test]
fn a_caller_never_passes_an_input_the_called_workflow_does_not_take() {
    for (caller, job, called, passed) in local_workflow_calls() {
        let declared = called_workflow_inputs(&called)
            .unwrap_or_else(|| panic!("{called} is called by {caller} but takes no workflow_call"));
        for key in passed.keys() {
            let key = key.as_str().expect("workflow input name is a string");
            assert!(
                declared.contains_key(key),
                "{caller} job `{job}` passes `{key}` to {called}, which does not take it"
            );
        }
    }
}

#[test]
fn a_caller_always_passes_every_input_the_called_workflow_requires() {
    for (caller, job, called, passed) in local_workflow_calls() {
        let declared = called_workflow_inputs(&called)
            .unwrap_or_else(|| panic!("{called} is called by {caller} but takes no workflow_call"));
        for (key, spec) in &declared {
            let key = key.as_str().expect("workflow input name is a string");
            let required = spec
                .as_mapping()
                .and_then(|spec| spec.get("required"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            assert!(
                !required || passed.contains_key(key),
                "{called} requires `{key}`, which {caller} job `{job}` does not pass"
            );
        }
    }
}

// One fan-out for every role. Five role workflows differing by one string is
// the duplication this catalog exists to remove.
#[test]
fn the_role_runner_reads_its_matrix_from_the_catalog() {
    let workflow = github_workflow("run.yml");
    let jobs = workflow_jobs(&workflow);
    assert_eq!(
        workflow_job_names(jobs),
        BTreeSet::from([
            "select".to_owned(),
            "run".to_owned(),
            "dependent".to_owned()
        ])
    );

    // The role and the kind reach the script as environment variables. A
    // workflow that pastes a caller's string into a shell command runs a
    // command the caller wrote, not one this repository reviewed.
    let render = named_step(
        workflow_job(jobs, "select"),
        "Render the jobs this role schedules",
    );
    let env = mapping_field(render, "env")
        .as_mapping()
        .expect("the render step has an environment");
    assert_eq!(
        mapping_field(env, "ROLE").as_str(),
        Some("${{ inputs.role }}")
    );
    assert_eq!(
        mapping_field(env, "KIND").as_str(),
        Some("${{ inputs.kind }}")
    );
    assert!(
        mapping_field(render, "run")
            .as_str()
            .unwrap_or_default()
            .contains(r#"just ci lanes --role "$ROLE" --kind "$KIND""#),
        "the selection comes from the catalog"
    );

    let text = github_workflow_text("run.yml");
    assert!(
        text.contains("uses: ./.github/workflows/lane.yml"),
        "the fan-out runs through the executor"
    );
    for job in ["run", "dependent"] {
        let with = mapping_field(workflow_job(jobs, job), "with")
            .as_mapping()
            .expect("the lane call passes inputs");
        assert_eq!(
            mapping_field(with, "runner").as_str(),
            Some("${{ matrix.runner || '' }}"),
            "the fan-out loses the lane's runner affinity"
        );
        assert!(
            with.get("isolated-target").is_none(),
            "the fan-out still decides where a lane builds"
        );
    }
    assert_eq!(
        job_needs(workflow_job(jobs, "dependent")),
        BTreeSet::from(["select".to_owned(), "run".to_owned()]),
        "a dependent lane runs after the matrix it reads"
    );
    let dependent = workflow_job(jobs, "dependent");
    let with = mapping_field(dependent, "with")
        .as_mapping()
        .expect("the dependent lane call passes inputs");
    for (name, value) in [
        ("artifact-name", "${{ matrix.artifact.name || '' }}"),
        ("artifact-path", "${{ matrix.artifact.path || '' }}"),
        ("artifact-when", "${{ matrix.artifact.when || 'always' }}"),
    ] {
        assert_eq!(
            mapping_field(with, name).as_str(),
            Some(value),
            "the dependent lane loses `{name}`"
        );
    }
}

#[cfg(unix)]
#[test]
fn failed_catalog_reads_publish_no_selection_outputs() -> anyhow::Result<()> {
    use std::process::Command;

    let workflow = github_workflow("run.yml");
    let render = named_step(
        workflow_job(workflow_jobs(&workflow), "select"),
        "Render the jobs this role schedules",
    );
    let script = mapping_field(render, "run")
        .as_str()
        .expect("the selection step runs a script");
    let temp = tempfile::tempdir()?;
    let outputs = temp.path().join("outputs");
    for fail_field in ["matrix", "dependent", ""] {
        fs::write(&outputs, "")?;
        let output = Command::new("bash")
            .args(["-euo", "pipefail", "-c"])
            .arg(format!(
                r#"just() {{
  field="${{@: -1}}"
  if [[ "$field" = "$FAIL_FIELD" ]]; then return 73; fi
  printf '[{{"lane":"%s"}}]\n' "$field"
}}
{script}"#
            ))
            .env("ROLE", "quality")
            .env("KIND", "branch")
            .env("ONLY", "all")
            .env("FAIL_FIELD", fail_field)
            .env("GITHUB_OUTPUT", &outputs)
            .output()?;
        if fail_field.is_empty() {
            assert!(output.status.success(), "{output:?}");
            assert_eq!(
                fs::read_to_string(&outputs)?,
                "matrix=[{\"lane\":\"matrix\"}]\ndependent=[{\"lane\":\"dependent\"}]\n"
            );
        } else {
            assert_eq!(output.status.code(), Some(73), "{output:?}");
            assert_eq!(fs::read_to_string(&outputs)?, "");
        }
    }
    Ok(())
}

/// `wreq` is the backend the Apple desktop app ships, and asking for it
/// builds `BoringSSL` through cmake. The Windows guest carries neither that
/// toolchain nor the product that would use the backend, so the lane names the
/// one the platform actually carries; without that the suite stops in a build
/// script, which is where it stopped the first time it ever got that far.
#[test]
fn the_windows_lane_runs_the_backend_the_platform_ships() {
    let workflow = github_workflow("windows.yml");
    let job = workflow_job(workflow_jobs(&workflow), "windows");
    let steps = mapping_field(job, "steps")
        .as_sequence()
        .expect("the lane lists steps");
    let tests = steps
        .iter()
        .filter_map(|step| step.get("run").and_then(Value::as_str))
        .find(|run| run.starts_with("just test"))
        .expect("the lane runs the suite");

    assert_eq!(
        tests, "just test run --net-backend=none",
        "the suite takes the default backend, which this guest cannot build"
    );
}

/// The guest carries `FFmpeg` and libclang, and the build scripts find them only
/// through these. Without `FFMPEG_DIR` the crate falls through to vcpkg and
/// then pkg-config, the guest has neither; without `LIBCLANG_PATH` bindgen
/// loads no library. Either way a build script panics before a single test
/// runs. Where they sit is decided by the script that installs them, so that
/// script says it, machine-wide, the way it says where Monkey's Audio is. The
/// libraries were once installed by hand and repository variables named
/// where; rebuilding the guest took the libraries and left the variables
/// pointing at nothing.
#[test]
fn the_guest_alone_says_where_its_libraries_are() {
    let workflow = github_workflow("windows.yml");
    let job = workflow_job(workflow_jobs(&workflow), "windows");
    let provision = fs::read_to_string(
        workflows_dir()
            .join("../../.config/windows/provision.ps1")
            .as_path(),
    )
    .expect("the guest's provisioning script is readable");

    for name in ["FFMPEG_DIR", "LIBCLANG_PATH"] {
        assert!(
            job.get("env").and_then(|env| env.get(name)).is_none(),
            "the lane restates `{name}`, which the guest already says"
        );
        assert!(
            provision.lines().any(|line| {
                line.contains(&format!("SetEnvironmentVariable('{name}',"))
                    && line.contains("'Machine'")
            }),
            "the guest's provisioning never says where `{name}` points"
        );
    }
}

/// Provisioning writes to the machine that serves the fleet, so it is never
/// something a push starts: it is started by hand, on the machine itself, and
/// one pass at a time.
#[test]
fn the_host_provisions_itself_only_when_someone_asks_it_to() {
    let workflow = github_workflow("host.yml");
    let triggers = mapping_field(workflow.as_mapping().expect("workflow is a mapping"), "on")
        .as_mapping()
        .expect("workflow triggers are a mapping");
    let triggers: BTreeSet<&str> = triggers
        .keys()
        .map(|key| key.as_str().expect("a trigger is named"))
        .collect();
    assert_eq!(triggers, BTreeSet::from(["workflow_dispatch"]));

    let job = workflow_job(workflow_jobs(&workflow), "provision");
    // A throwaway runner container holds neither the Docker daemon that keeps
    // the images nor the systemd that owns the units.
    assert_eq!(
        mapping_field(job, "runs-on").as_str(),
        Some("${{ vars.KITHARA_HOST_RUNNER_LABEL }}")
    );
    assert_eq!(
        workflow_concurrency(&workflow)
            .get("cancel-in-progress")
            .and_then(Value::as_bool),
        Some(false),
        "a second pass would rebuild what the first is installing against"
    );
}

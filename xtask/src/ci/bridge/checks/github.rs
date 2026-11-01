use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use serde_json::Value;

use super::projection::{Snapshot, check, check_context, check_contexts};
use crate::ci::bridge::{
    api::Github,
    model::{CheckStatus, PullRequest},
};

pub(super) fn observe(github: &Github, pull: &PullRequest) -> Result<Snapshot> {
    let repository = pull
        .head_repository
        .as_deref()
        .context("PR source repository no longer exists")?;
    observe_current_run(
        pull,
        || github.workflow_runs(repository, &pull.head_sha),
        |run_id, attempt| github.workflow_jobs(repository, run_id, attempt),
        || {
            Ok((
                github.source_statuses(repository, &pull.head_sha)?,
                github.source_check_runs(repository, &pull.head_sha)?,
            ))
        },
    )
}

fn observe_current_run(
    pull: &PullRequest,
    mut read_runs: impl FnMut() -> Result<Vec<Value>>,
    mut read_jobs: impl FnMut(u64, u64) -> Result<Vec<Value>>,
    read_published: impl FnOnce() -> Result<(Vec<Value>, Vec<Value>)>,
) -> Result<Snapshot> {
    let runs = read_runs()?;
    let (published_statuses, checks) = read_published()?;
    let run = latest_run(&runs, pull)?;
    let mut statuses = if let Some(run) = run {
        let run_id = run["id"]
            .as_u64()
            .context("GitHub workflow run has no id")?;
        let attempt = run["run_attempt"]
            .as_u64()
            .context("GitHub workflow run has no attempt")?;
        snapshot(run, &read_jobs(run_id, attempt)?, &checks)?.statuses
    } else {
        Vec::new()
    };
    for current in latest_source_runs(&runs, pull)?.into_values() {
        if run.is_some_and(|run| same_run(run, current)) {
            continue;
        }
        let run_id = current["id"]
            .as_u64()
            .context("GitHub workflow run has no id")?;
        let attempt = current["run_attempt"]
            .as_u64()
            .context("GitHub workflow run has no attempt")?;
        statuses.extend(workflow_checks(
            current,
            &read_jobs(run_id, attempt)?,
            &checks,
        )?);
    }
    statuses.extend(published_checks(pull, &runs, &published_statuses, &checks)?);
    let current_runs = read_runs()?;
    let previous = latest_source_runs(&runs, pull)?;
    let current = latest_source_runs(&current_runs, pull)?;
    if previous.len() != current.len()
        || previous.iter().any(|(workflow, run)| {
            current
                .get(workflow)
                .is_none_or(|current| !same_run(run, current))
        })
    {
        return Ok(pending(
            latest_run(&current_runs, pull)?,
            "Source CI changed while reading checks; waiting for its current run",
        ));
    }
    let url = run.and_then(|run| run["html_url"].as_str());
    Ok(source_summary(statuses, url))
}

fn source_summary(mut statuses: Vec<CheckStatus>, url: Option<&str>) -> Snapshot {
    let complete = !statuses.is_empty() && statuses.iter().all(|status| status.state != "pending");
    let (state, detail) = if !complete {
        (
            "pending",
            "Waiting for reported source checks on the current PR head",
        )
    } else if statuses
        .iter()
        .any(|status| matches!(status.state, "failure" | "error"))
    {
        (
            "failure",
            "Reported source checks include failures; open their linked details",
        )
    } else {
        (
            "success",
            "All reported source checks succeeded on the current PR head",
        )
    };
    let detail = if state == "failure" {
        statuses
            .iter()
            .find(|status| status.context == "kithara/github-ci" && status.state == "failure")
            .map_or_else(|| detail.to_owned(), |status| status.description.clone())
    } else {
        detail.to_owned()
    };
    statuses.retain(|status| status.context != "kithara/github-ci");
    statuses.push(check("kithara/github-ci", state, detail, url));
    Snapshot {
        statuses,
        complete,
        target_url: url.map(str::to_owned),
    }
}

fn latest_source_runs<'a>(
    runs: &'a [Value],
    pull: &PullRequest,
) -> Result<BTreeMap<u64, &'a Value>> {
    let mut latest: BTreeMap<u64, &Value> = BTreeMap::new();
    for run in runs.iter().filter(|run| matches_source(run, pull)) {
        let workflow = run["workflow_id"]
            .as_u64()
            .context("GitHub workflow run has no workflow id")?;
        let id = run["id"]
            .as_u64()
            .context("GitHub workflow run has no id")?;
        run["check_suite_id"]
            .as_u64()
            .context("GitHub workflow run has no check suite id")?;
        if latest
            .get(&workflow)
            .is_none_or(|previous| previous["id"].as_u64() < Some(id))
        {
            latest.insert(workflow, run);
        }
    }
    Ok(latest)
}

fn published_checks(
    pull: &PullRequest,
    runs: &[Value],
    statuses: &[Value],
    checks: &[Value],
) -> Result<Vec<CheckStatus>> {
    let mut projected = commit_statuses(statuses)?;
    projected.extend(check_runs(pull, runs, checks)?);
    let names = projected
        .iter()
        .map(|status| status.context.as_str())
        .collect::<Vec<_>>();
    let contexts = check_contexts("GitHub", &names);
    for (status, context) in projected.iter_mut().zip(contexts) {
        status.context = context;
    }
    Ok(projected)
}

fn workflow_checks(run: &Value, jobs: &[Value], checks: &[Value]) -> Result<Vec<CheckStatus>> {
    let workflow = run["workflow_id"]
        .as_u64()
        .context("GitHub workflow run has no workflow id")?;
    let name = run["name"]
        .as_str()
        .context("GitHub workflow run has no name")?;
    let mut statuses = workflow_job_checks(run, jobs, checks)?;
    let (state, detail) = job_state(run)?;
    let label = format!("workflow: Actions {workflow} ({name})");
    for context in check_contexts("GitHub", &[label.as_str()]) {
        statuses.push(check(context, state, detail, run["html_url"].as_str()));
    }
    Ok(statuses)
}

fn workflow_job_checks(run: &Value, jobs: &[Value], checks: &[Value]) -> Result<Vec<CheckStatus>> {
    let workflow = run["workflow_id"]
        .as_u64()
        .context("GitHub workflow run has no workflow id")?;
    let name = run["name"]
        .as_str()
        .context("GitHub workflow run has no name")?;
    let mut counts = BTreeMap::new();
    for job in jobs {
        let label = job["name"].as_str().context("GitHub job has no name")?;
        *counts.entry(label).or_insert(0) += 1;
    }
    jobs.iter()
        .map(|job| {
            validate_job(run, job)?;
            let job_name = job["name"].as_str().context("GitHub job has no name")?;
            let mut label = format!("job: Actions workflow {workflow} ({name}): {job_name}");
            let duplicate = if counts[job_name] > 1 {
                Some(
                    job["id"]
                        .as_u64()
                        .context("Same-named GitHub job has no id")?,
                )
            } else {
                None
            };
            if let Some(id) = duplicate {
                label.push_str(&format!(" [job {id}]"));
            }
            let identity = serde_json::to_vec(&("actions-job", workflow, job_name, duplicate))?;
            let context = check_context("GitHub", &label, &identity);
            let (state, conclusion) = job_state(job)?;
            let title = job["check_run_url"]
                .as_str()
                .and_then(|url| {
                    checks
                        .iter()
                        .find(|check| check["url"].as_str() == Some(url))
                })
                .and_then(|check| check.pointer("/output/title"))
                .and_then(Value::as_str);
            let detail = check_detail(conclusion, title);
            Ok(check(context, state, detail, job["html_url"].as_str()))
        })
        .collect()
}

fn commit_statuses(statuses: &[Value]) -> Result<Vec<CheckStatus>> {
    let mut latest: BTreeMap<String, (u64, &str, &Value)> = BTreeMap::new();
    for status in statuses {
        let context = status["context"]
            .as_str()
            .context("GitHub source status has no context")?;
        let identity = context.to_lowercase();
        if matches!(
            identity.as_str(),
            "kithara/pr-size" | "kithara/github-ci" | "kithara/gitlab-verification"
        ) || identity.starts_with("github /")
            || identity.starts_with("gitlab /")
        {
            continue;
        }
        let id = status["id"]
            .as_u64()
            .context("GitHub source status has no id")?;
        if latest
            .get(&identity)
            .is_none_or(|(previous, _, _)| *previous < id)
        {
            latest.insert(identity, (id, context, status));
        }
    }
    latest
        .into_values()
        .map(|(_, context, status)| {
            let state = match status["state"].as_str() {
                Some("success") => "success",
                Some("failure") => "failure",
                Some("error") => "error",
                Some("pending") => "pending",
                _ => bail!("GitHub source status has an unknown state"),
            };
            Ok(check(
                format!("status: {context}"),
                state,
                status["description"].as_str().unwrap_or(state),
                status["target_url"].as_str(),
            ))
        })
        .collect()
}

fn check_runs(pull: &PullRequest, runs: &[Value], checks: &[Value]) -> Result<Vec<CheckStatus>> {
    let mut latest: BTreeMap<(u64, &str), (u64, &Value)> = BTreeMap::new();
    for check in checks {
        if check["head_sha"].as_str() != Some(pull.head_sha.as_str()) {
            bail!("GitHub check-run belongs to another PR head");
        }
        let suite = check
            .pointer("/check_suite/id")
            .and_then(Value::as_u64)
            .context("GitHub check-run has no suite id")?;
        if runs
            .iter()
            .any(|run| run["check_suite_id"].as_u64() == Some(suite))
        {
            continue;
        }
        let app = check
            .pointer("/app/id")
            .and_then(Value::as_u64)
            .context("GitHub check-run has no app id")?;
        let name = check["name"]
            .as_str()
            .context("GitHub check-run has no name")?;
        let id = check["id"].as_u64().context("GitHub check-run has no id")?;
        let key = (app, name);
        if latest.get(&key).is_none_or(|(previous, _)| *previous < id) {
            latest.insert(key, (id, check));
        }
    }
    latest
        .into_iter()
        .map(|((app, name), (_, value))| {
            let provider = value
                .pointer("/app/name")
                .and_then(Value::as_str)
                .context("GitHub check-run has no app name")?;
            let (state, conclusion) = job_state(value)?;
            let detail = check_detail(
                conclusion,
                value.pointer("/output/title").and_then(Value::as_str),
            );
            Ok(check(
                format!("check: app {app} ({provider}): {name}"),
                state,
                detail,
                value["details_url"]
                    .as_str()
                    .or_else(|| value["html_url"].as_str()),
            ))
        })
        .collect()
}

fn check_detail(conclusion: &str, title: Option<&str>) -> String {
    title.filter(|title| !title.is_empty()).map_or_else(
        || conclusion.to_owned(),
        |title| format!("{conclusion}: {title}"),
    )
}

fn same_run(previous: &Value, current: &Value) -> bool {
    ["id", "run_attempt", "status", "conclusion"]
        .iter()
        .all(|field| previous[*field] == current[*field])
}

fn pending(run: Option<&Value>, detail: &str) -> Snapshot {
    let url = run.and_then(|run| run["html_url"].as_str());
    Snapshot {
        statuses: vec![check("kithara/github-ci", "pending", detail, url)],
        complete: false,
        target_url: url.map(str::to_owned),
    }
}

fn latest_run<'a>(runs: &'a [Value], pull: &PullRequest) -> Result<Option<&'a Value>> {
    let mut latest = None;
    let mut latest_id = 0;
    for run in runs.iter().filter(|run| matches_pull(run, pull)) {
        let id = run["id"]
            .as_u64()
            .context("GitHub workflow run has no id")?;
        if id > latest_id {
            latest = Some(run);
            latest_id = id;
        }
    }
    Ok(latest)
}

fn matches_pull(run: &Value, pull: &PullRequest) -> bool {
    matches_source(run, pull) && run["path"].as_str() == Some(".github/workflows/ci.yml")
}

fn matches_source(run: &Value, pull: &PullRequest) -> bool {
    run["head_sha"].as_str() == Some(pull.head_sha.as_str())
        && run["head_branch"].as_str() == Some(pull.head_branch.as_str())
        && run
            .pointer("/head_repository/full_name")
            .and_then(Value::as_str)
            == pull.head_repository.as_deref()
}

fn snapshot(run: &Value, jobs: &[Value], checks: &[Value]) -> Result<Snapshot> {
    let complete = run["status"].as_str() == Some("completed");
    let url = run["html_url"].as_str();
    let mut statuses = workflow_job_checks(run, jobs, checks)?;
    let required = jobs
        .iter()
        .find(|job| job["name"].as_str() == Some("CI required"));
    let required_succeeded = required.is_some_and(|job| {
        job["status"].as_str() == Some("completed") && job["conclusion"].as_str() == Some("success")
    });
    let state = if !complete {
        "pending"
    } else if run["conclusion"].as_str() == Some("success") && required_succeeded {
        "success"
    } else {
        "failure"
    };
    let detail = match state {
        "pending" => "Source CI is running",
        "success" => "Source CI and CI required succeeded on the current PR head",
        _ => "Source CI did not complete successfully with an executed CI required",
    };
    statuses.push(check("kithara/github-ci", state, detail, url));
    Ok(Snapshot {
        statuses,
        complete,
        target_url: url.map(str::to_owned),
    })
}

fn validate_job(run: &Value, job: &Value) -> Result<()> {
    let run_id = run["id"]
        .as_u64()
        .context("GitHub workflow run has no id")?;
    let attempt = run["run_attempt"]
        .as_u64()
        .context("GitHub workflow run has no attempt")?;
    if job["run_id"].as_u64() != Some(run_id) || job["run_attempt"].as_u64() != Some(attempt) {
        bail!("GitHub job belongs to another workflow run or attempt");
    }
    Ok(())
}

fn job_state(job: &Value) -> Result<(&'static str, &str)> {
    let status = job["status"].as_str().context("GitHub job has no status")?;
    if status != "completed" {
        return Ok(("pending", status));
    }
    let conclusion = job["conclusion"]
        .as_str()
        .context("Completed GitHub job has no conclusion")?;
    Ok((
        match conclusion {
            "success" | "skipped" | "neutral" => "success",
            _ => "failure",
        },
        conclusion,
    ))
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use serde_json::json;

    use super::*;
    use crate::ci::bridge::model::Scope;

    fn pull() -> PullRequest {
        PullRequest {
            number: 763,
            head_sha: "a".repeat(40),
            head_repository: Some("fork/kithara".into()),
            head_branch: "feature".into(),
            scope: Scope::Full,
            author: "fork".into(),
        }
    }

    fn run(id: u64, status: &str, conclusion: &str) -> Value {
        json!({ "id": id, "workflow_id": 10, "check_suite_id": id * 10, "name": "CI", "run_attempt": 2, "head_sha": "a".repeat(40), "head_branch": "feature", "head_repository": {"full_name":"fork/kithara"}, "path":".github/workflows/ci.yml", "event":"push", "status":status, "conclusion":conclusion, "html_url":"https://github.com/fork/kithara/actions/runs/2" })
    }

    fn job(name: &str, conclusion: &str) -> Value {
        json!({ "id": 1, "name": name, "run_id": 2, "run_attempt": 2, "status":"completed", "conclusion":conclusion, "html_url": "https://github.com/fork/kithara/actions/runs/2/job/1" })
    }

    fn workflow_job(run_id: u64, attempt: u64, name: &str, conclusion: &str) -> Value {
        let mut value = job(name, conclusion);
        value["run_id"] = json!(run_id);
        value["run_attempt"] = json!(attempt);
        value
    }

    fn status(id: u64, context: &str, state: &str) -> Value {
        json!({"id": id, "context": context, "state": state, "description": "compiler rejected crate", "target_url": "https://ci.contributor.example/job/42"})
    }

    fn provider_check(id: u64, name: &str, conclusion: &str) -> Value {
        json!({"id": id, "head_sha": "a".repeat(40), "app": {"id": 99, "name": "Contributor CI"}, "check_suite": {"id": 999}, "name": name, "status": "completed", "conclusion": conclusion, "output": {"title": "tests rejected current head"}, "details_url": "https://ci.contributor.example/check/42"})
    }

    fn custom_run(id: u64, workflow: u64, state: &str, conclusion: &str) -> Value {
        let mut value = run(id, state, conclusion);
        value["workflow_id"] = json!(workflow);
        value["name"] = json!(format!("Custom {workflow}"));
        value["path"] = json!(format!(".github/workflows/custom-{workflow}.yml"));
        value
    }

    fn observe_values(
        pull: &PullRequest,
        runs: &[Value],
        jobs: &[Value],
        statuses: &[Value],
        checks: &[Value],
    ) -> Result<Snapshot> {
        observe_current_run(
            pull,
            || Ok(runs.to_vec()),
            |run_id, _| {
                Ok(jobs
                    .iter()
                    .filter(|job| job["run_id"].as_u64() == Some(run_id))
                    .cloned()
                    .collect())
            },
            || Ok((statuses.to_vec(), checks.to_vec())),
        )
    }

    fn aggregate(snapshot: &Snapshot) -> &str {
        snapshot
            .statuses
            .iter()
            .find(|status| status.context == "kithara/github-ci")
            .unwrap()
            .state
    }

    #[test]
    fn the_latest_source_ci_run_must_match_head_branch_repository_and_workflow() {
        let correct = run(2, "completed", "failure");
        let mut unrelated = run(3, "completed", "success");
        unrelated["path"] = json!(".github/workflows/unrelated.yml");
        let runs = [run(1, "completed", "success"), correct, unrelated];
        assert_eq!(latest_run(&runs, &pull()).unwrap().unwrap()["id"], 2);
        for field in ["head_sha", "head_branch"] {
            let mut wrong = run(4, "completed", "success");
            wrong[field] = json!("different");
            assert!(latest_run(&[wrong], &pull()).unwrap().is_none());
        }
        let mut wrong = run(4, "completed", "success");
        wrong["head_repository"]["full_name"] = json!("someone/else");
        assert!(latest_run(&[wrong], &pull()).unwrap().is_none());
        assert!(latest_run(&[], &pull()).unwrap().is_none());
    }

    #[test]
    fn success_requires_both_workflow_and_executed_required_job_success() {
        let completed = run(2, "completed", "success");
        assert_eq!(
            aggregate(&snapshot(&completed, &[job("CI required", "success")], &[]).unwrap()),
            "success"
        );
        assert_eq!(
            aggregate(&snapshot(&completed, &[], &[]).unwrap()),
            "failure"
        );
        for conclusion in ["failure", "cancelled", "timed_out", "skipped", "neutral"] {
            let snapshot = snapshot(&completed, &[job("CI required", conclusion)], &[]).unwrap();
            assert_eq!(aggregate(&snapshot), "failure", "{conclusion}");
        }
        let failed = run(2, "completed", "failure");
        assert_eq!(
            aggregate(&snapshot(&failed, &[job("CI required", "success")], &[]).unwrap()),
            "failure"
        );
    }

    #[test]
    fn a_rerun_cannot_reuse_an_older_attempt_or_green_jobs_to_finish_early() {
        let running = run(2, "in_progress", "");
        assert_eq!(
            aggregate(&snapshot(&running, &[job("CI required", "success")], &[]).unwrap()),
            "pending"
        );
        let mut previous = job("CI required", "success");
        previous["run_attempt"] = json!(1);
        assert!(snapshot(&running, &[previous], &[]).is_err());
    }

    #[test]
    fn a_run_attempt_or_verdict_change_during_job_listing_cannot_publish_stale_success() {
        let completed = run(2, "completed", "success");
        let mut next_attempt = completed.clone();
        next_attempt["run_attempt"] = json!(3);
        let mut different_conclusion = completed.clone();
        different_conclusion["conclusion"] = json!("failure");
        for current in [
            Some(run(3, "in_progress", "")),
            Some(next_attempt),
            Some(run(2, "in_progress", "")),
            Some(different_conclusion),
            None,
        ] {
            let reads = RefCell::new(Vec::new());
            let mut listed = [vec![completed.clone()], current.into_iter().collect()].into_iter();
            let observed = observe_current_run(
                &pull(),
                || {
                    reads.borrow_mut().push("runs");
                    Ok(listed.next().expect("exactly two listing reads"))
                },
                |run_id, attempt| {
                    reads.borrow_mut().push("jobs");
                    assert_eq!((run_id, attempt), (2, 2));
                    Ok(vec![job("CI required", "success")])
                },
                || Ok((Vec::new(), Vec::new())),
            )
            .unwrap();
            assert_eq!(*reads.borrow(), ["runs", "jobs", "runs"]);
            assert_eq!(aggregate(&observed), "pending");
            assert!(!observed.complete);
            assert_eq!(observed.statuses.len(), 1);
            assert!(
                observed
                    .statuses
                    .iter()
                    .all(|status| status.state != "success")
            );
        }
    }

    #[test]
    fn an_unchanged_run_observed_after_its_jobs_can_publish_success() {
        let completed = run(2, "completed", "success");
        let reads = RefCell::new(Vec::new());
        let observed = observe_current_run(
            &pull(),
            || {
                reads.borrow_mut().push("runs");
                Ok(vec![completed.clone()])
            },
            |run_id, attempt| {
                reads.borrow_mut().push("jobs");
                assert_eq!((run_id, attempt), (2, 2));
                Ok(vec![job("CI required", "success")])
            },
            || Ok((Vec::new(), Vec::new())),
        )
        .unwrap();
        assert_eq!(*reads.borrow(), ["runs", "jobs", "runs"]);
        assert_eq!(aggregate(&observed), "success");
        assert!(observed.complete);
        assert_eq!(observed.statuses.len(), 2);
    }

    #[test]
    fn skipped_optional_jobs_are_explicit_while_failed_jobs_stay_red() {
        let snapshot = snapshot(
            &run(2, "completed", "failure"),
            &[
                job("lint", "failure"),
                job("optional", "skipped"),
                job("CI required", "failure"),
            ],
            &[],
        )
        .unwrap();
        assert_eq!(snapshot.statuses[0].state, "failure");
        assert_eq!(snapshot.statuses[1].state, "success");
        assert_eq!(snapshot.statuses[1].description, "skipped");
        assert_eq!(aggregate(&snapshot), "failure");
    }

    #[test]
    fn unrelated_forks_and_self_hosted_runners_need_no_source_configuration() {
        for repository in ["contributor-one/kithara", "another-team/audio-fork"] {
            let mut pull = pull();
            pull.head_repository = Some(repository.to_owned());
            let mut own_run = run(2, "completed", "success");
            own_run["head_repository"]["full_name"] = json!(repository);
            own_run["runner_name"] = json!(format!("{repository}-own-server"));
            let observed = observe_values(
                &pull,
                &[own_run.clone()],
                &[job("CI required", "success")],
                &[],
                &[],
            )
            .unwrap();
            assert_eq!(aggregate(&observed), "success");
            own_run["head_repository"]["full_name"] = json!("different/fork");
            assert_eq!(
                aggregate(&observe_values(&pull, &[own_run], &[], &[], &[]).unwrap()),
                "pending"
            );
        }
    }

    #[test]
    fn external_servers_publish_independent_results_and_supplied_diagnostics() {
        let observed = observe_values(
            &pull(),
            &[],
            &[],
            &[status(1, "Jenkins / build", "failure")],
            &[provider_check(1, "mac-tests", "success")],
        )
        .unwrap();
        assert_eq!(aggregate(&observed), "failure");
        assert!(observed.complete);
        let failed = observed
            .statuses
            .iter()
            .find(|status| status.context.contains("Jenkins"))
            .unwrap();
        assert_eq!(failed.state, "failure");
        assert_eq!(failed.description, "compiler rejected crate");
        assert_eq!(
            failed.target_url.as_deref(),
            Some("https://ci.contributor.example/job/42")
        );
        let passed = observed
            .statuses
            .iter()
            .find(|status| status.context.contains("mac-tests"))
            .unwrap();
        assert_eq!(passed.state, "success");
        assert_eq!(passed.description, "success: tests rejected current head");
        assert_eq!(
            passed.target_url.as_deref(),
            Some("https://ci.contributor.example/check/42")
        );
        assert!(
            observed
                .statuses
                .last()
                .unwrap()
                .description
                .contains("Reported source checks")
        );
    }

    #[test]
    fn source_status_contexts_use_github_case_insensitive_identity() {
        let statuses = [
            status(3, "SERVER/Test", "success"),
            status(1, "server/test", "failure"),
            status(2, "Server/TEST", "pending"),
            status(4, "KITHARA/GITHUB-CI", "failure"),
            status(5, "gItHuB / previous", "failure"),
            status(6, "GITLAB / previous", "failure"),
        ];
        let observed = observe_values(&pull(), &[], &[], &statuses, &[]).unwrap();
        assert_eq!(aggregate(&observed), "success");
        assert_eq!(observed.statuses.len(), 2);
        assert!(observed.statuses[0].context.contains("SERVER/Test"));
        assert_eq!(observed.statuses[0].state, "success");
    }

    #[test]
    fn retry_results_select_the_latest_context_and_provider_check_not_feed_order() {
        let statuses = [
            status(3, "server/test", "success"),
            status(1, "server/test", "failure"),
            status(2, "server/test", "pending"),
        ];
        let checks = [
            provider_check(3, "tests", "failure"),
            provider_check(1, "tests", "success"),
            provider_check(2, "tests", "success"),
        ];
        let failed = observe_values(&pull(), &[], &[], &statuses, &checks).unwrap();
        assert_eq!(aggregate(&failed), "failure");
        assert_eq!(failed.statuses.len(), 3);
        let latest = provider_check(4, "tests", "success");
        let passed = observe_values(
            &pull(),
            &[],
            &[],
            &statuses,
            &[checks.to_vec(), vec![latest]].concat(),
        )
        .unwrap();
        assert_eq!(aggregate(&passed), "success");
        assert!(
            passed
                .statuses
                .last()
                .unwrap()
                .description
                .contains("reported source checks")
        );
        let pending = observe_values(
            &pull(),
            &[],
            &[],
            &[status(4, "server/test", "pending")],
            &checks,
        )
        .unwrap();
        assert_eq!(aggregate(&pending), "pending");
        assert!(
            pending
                .statuses
                .iter()
                .any(|status| status.state == "failure")
        );
        assert!(!pending.complete);
    }

    #[test]
    fn native_workflow_identity_separates_same_named_jobs_and_drops_stale_suites() {
        let old = custom_run(1, 11, "completed", "failure");
        let first = custom_run(2, 11, "completed", "success");
        let second = custom_run(3, 12, "completed", "failure");
        let mut stale = provider_check(100, "test", "failure");
        stale["check_suite"]["id"] = json!(10);
        let mut first_check = provider_check(101, "test", "success");
        first_check["check_suite"]["id"] = json!(20);
        let mut second_check = provider_check(102, "test", "failure");
        second_check["check_suite"]["id"] = json!(30);
        let observed = observe_values(
            &pull(),
            &[old, first, second],
            &[
                workflow_job(2, 2, "test", "success"),
                workflow_job(3, 2, "test", "failure"),
            ],
            &[],
            &[stale, first_check, second_check],
        )
        .unwrap();
        let jobs = observed
            .statuses
            .iter()
            .filter(|status| status.context.contains("job:"))
            .collect::<Vec<_>>();
        assert_eq!(jobs.len(), 2);
        assert_ne!(jobs[0].context, jobs[1].context);
        assert!(
            jobs.iter()
                .any(|job| job.context.contains("workflow 11") && job.state == "success")
        );
        assert!(
            jobs.iter()
                .any(|job| job.context.contains("workflow 12") && job.state == "failure")
        );
        assert_eq!(aggregate(&observed), "failure");
    }

    #[test]
    fn a_nonstandard_workflow_retry_stays_pending_before_new_checks_appear() {
        let previous = custom_run(1, 11, "completed", "success");
        let mut retry = custom_run(2, 11, "in_progress", "");
        retry["run_attempt"] = json!(3);
        let mut old_check = provider_check(1, "test", "success");
        old_check["check_suite"]["id"] = json!(10);
        let observed =
            observe_values(&pull(), &[previous, retry.clone()], &[], &[], &[old_check]).unwrap();
        assert_eq!(aggregate(&observed), "pending");
        assert_eq!(observed.statuses.len(), 2);
        let mut same_suite_old_check = provider_check(2, "test", "success");
        same_suite_old_check["check_suite"]["id"] = json!(20);
        let observed =
            observe_values(&pull(), &[retry], &[], &[], &[same_suite_old_check]).unwrap();
        assert_eq!(aggregate(&observed), "pending");
        assert!(!observed.complete);
    }

    #[test]
    fn completed_retry_uses_current_attempt_jobs_and_retires_old_same_suite_checks() {
        let mut retry = custom_run(2, 11, "completed", "success");
        retry["run_attempt"] = json!(3);
        let mut removed = provider_check(50, "removed", "failure");
        removed["check_suite"]["id"] = json!(20);
        let current = workflow_job(2, 3, "retained", "success");
        let observed =
            observe_values(&pull(), &[retry.clone()], &[current], &[], &[removed]).unwrap();
        assert_eq!(aggregate(&observed), "success");
        assert!(
            observed
                .statuses
                .iter()
                .any(|status| status.context.contains("retained"))
        );
        assert!(
            observed
                .statuses
                .iter()
                .all(|status| !status.context.contains("removed"))
        );
        assert!(
            observe_values(
                &pull(),
                &[retry],
                &[workflow_job(2, 2, "retained", "success")],
                &[],
                &[]
            )
            .is_err()
        );
    }

    #[test]
    fn standard_required_gate_cannot_be_replaced_or_duplicated_by_other_providers() {
        for required in [Vec::new(), vec![job("CI required", "skipped")]] {
            let mut native = provider_check(1, "CI required", "success");
            native["check_suite"]["id"] = json!(20);
            let observed = observe_values(
                &pull(),
                &[run(2, "completed", "success")],
                &required,
                &[status(1, "external", "success")],
                &[native, provider_check(2, "external", "success")],
            )
            .unwrap();
            assert_eq!(aggregate(&observed), "failure");
            assert_eq!(
                observed
                    .statuses
                    .iter()
                    .filter(|status| status.context.contains("check:"))
                    .count(),
                1
            );
            assert!(
                observed
                    .statuses
                    .iter()
                    .any(|status| status.context.contains("external") && status.state == "success")
            );
        }
    }

    #[test]
    fn every_observed_standard_workflow_event_requires_an_executed_gate() {
        for event in ["schedule", "pull_request", "push", "workflow_dispatch"] {
            let mut observed = run(2, "completed", "success");
            observed["event"] = json!(event);
            let skipped = observe_values(
                &pull(),
                &[observed.clone()],
                &[job("CI required", "skipped")],
                &[],
                &[],
            )
            .unwrap();
            assert_eq!(aggregate(&skipped), "failure", "{event}");
            let executed = observe_values(
                &pull(),
                &[observed],
                &[job("CI required", "success")],
                &[],
                &[],
            )
            .unwrap();
            assert_eq!(aggregate(&executed), "success", "{event}");
        }
    }

    #[test]
    fn native_jobs_cannot_collide_with_external_statuses_or_provider_checks() {
        let observed = observe_values(
            &pull(),
            &[run(2, "completed", "success")],
            &[
                job("CI required", "success"),
                job("status: smoke", "failure"),
            ],
            &[status(1, "smoke", "success")],
            &[provider_check(1, "status: smoke", "success")],
        )
        .unwrap();
        let contexts = observed
            .statuses
            .iter()
            .map(|status| &status.context)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(contexts.len(), observed.statuses.len());
        assert!(
            observed
                .statuses
                .iter()
                .any(|status| status.context.contains("job:") && status.state == "failure")
        );
        assert!(
            observed
                .statuses
                .iter()
                .any(|status| status.context.contains("status:") && status.state == "success")
        );
    }

    #[test]
    fn literal_job_names_cannot_impersonate_duplicate_job_discriminators() {
        let mut first = workflow_job(2, 2, "test", "failure");
        first["id"] = json!(1);
        let mut second = workflow_job(2, 2, "test", "success");
        second["id"] = json!(2);
        let mut literal = workflow_job(2, 2, "test [job 1]", "success");
        literal["id"] = json!(3);
        let observed = observe_values(
            &pull(),
            &[custom_run(2, 11, "completed", "success")],
            &[first, second, literal],
            &[],
            &[],
        )
        .unwrap();
        let contexts = observed
            .statuses
            .iter()
            .map(|status| status.context.to_lowercase())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(contexts.len(), observed.statuses.len());
        assert_eq!(aggregate(&observed), "failure");
        assert_eq!(
            observed
                .statuses
                .iter()
                .filter(|status| status.context.contains("job:"))
                .count(),
            3
        );
    }

    #[test]
    fn workflow_identity_survives_slashes_long_names_and_same_named_native_jobs() {
        let mut first = custom_run(2, 11, "completed", "success");
        first["name"] = json!("CI / Linux");
        let mut second = custom_run(3, 12, "completed", "success");
        second["name"] = json!(format!("CI / {}", "Mac".repeat(100)));
        let mut duplicate = workflow_job(3, 2, "matrix / test", "success");
        duplicate["id"] = json!(2);
        let observed = observe_values(
            &pull(),
            &[first, second],
            &[
                workflow_job(2, 2, "matrix / test", "success"),
                workflow_job(3, 2, "matrix / test", "success"),
                duplicate,
            ],
            &[],
            &[],
        )
        .unwrap();
        let contexts = observed
            .statuses
            .iter()
            .map(|status| &status.context)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(contexts.len(), observed.statuses.len());
        assert!(
            observed
                .statuses
                .iter()
                .all(|status| status.context.chars().count() <= 100)
        );
        assert_eq!(
            observed
                .statuses
                .iter()
                .filter(|status| status.context.contains("job:"))
                .count(),
            3
        );
        assert_eq!(aggregate(&observed), "success");
    }

    #[test]
    fn wrong_head_checks_and_wrong_branch_actions_cannot_prove_success() {
        let mut wrong = provider_check(1, "test", "success");
        wrong["head_sha"] = json!("b".repeat(40));
        assert!(observe_values(&pull(), &[], &[], &[], &[wrong]).is_err());
        for field in ["head_branch", "head_sha"] {
            let mut wrong = custom_run(2, 11, "completed", "success");
            wrong[field] = json!("different");
            let mut check = provider_check(1, "test", "success");
            check["check_suite"]["id"] = json!(20);
            assert_eq!(
                aggregate(&observe_values(&pull(), &[wrong], &[], &[], &[check]).unwrap()),
                "pending"
            );
        }
    }

    #[test]
    fn reserved_mirror_statuses_are_excluded_but_native_checks_keep_their_names() {
        let mirrors = [
            "kithara/pr-size",
            "kithara/github-ci",
            "kithara/gitlab-verification",
            "GitHub / previous",
            "GitLab / previous",
        ]
        .map(|context| status(1, context, "success"));
        assert_eq!(
            aggregate(&observe_values(&pull(), &[], &[], &mirrors, &[]).unwrap()),
            "pending"
        );
        let native = observe_values(
            &pull(),
            &[],
            &[],
            &mirrors,
            &[provider_check(1, "kithara/github-ci", "failure")],
        )
        .unwrap();
        assert_eq!(aggregate(&native), "failure");
        assert!(
            native
                .statuses
                .iter()
                .any(|status| status.context.starts_with("GitHub / check:")
                    && status.context.contains("kithara/github-ci"))
        );
        assert_eq!(
            aggregate(
                &observe_values(
                    &pull(),
                    &[],
                    &[],
                    &[status(1, "kithara/custom-ci", "success")],
                    &[]
                )
                .unwrap()
            ),
            "success"
        );
    }

    #[test]
    fn a_missing_diagnostic_link_or_title_does_not_invent_a_cause_or_destination() {
        let mut failed_status = status(1, "bare-status", "failure");
        failed_status["description"] = Value::Null;
        failed_status["target_url"] = Value::Null;
        let mut failed_check = provider_check(1, "bare-check", "failure");
        failed_check["output"]["title"] = Value::Null;
        failed_check["details_url"] = Value::Null;
        let observed =
            observe_values(&pull(), &[], &[], &[failed_status], &[failed_check]).unwrap();
        assert_eq!(aggregate(&observed), "failure");
        for status in observed
            .statuses
            .iter()
            .filter(|status| status.context.starts_with("GitHub /"))
        {
            assert_eq!(status.description, "failure");
            assert!(status.target_url.is_none());
        }
    }

    #[test]
    fn native_diagnostics_use_only_the_exact_current_jobs_check_run() {
        let mut current = job("tests", "failure");
        current["check_run_url"] =
            json!("https://api.github.com/repos/fork/kithara/check-runs/200");
        let mut matching = provider_check(200, "tests", "failure");
        matching["check_suite"]["id"] = json!(20);
        matching["url"] = current["check_run_url"].clone();
        matching["output"]["title"] = json!("assertion failed in replay");
        let mut previous = provider_check(199, "tests", "failure");
        previous["check_suite"]["id"] = json!(20);
        previous["url"] = json!("https://api.github.com/repos/fork/kithara/check-runs/199");
        previous["output"]["title"] = json!("old attempt diagnostic");
        let checks = [previous, matching];
        let observed = observe_values(
            &pull(),
            &[run(2, "completed", "failure")],
            &[job("CI required", "success"), current.clone()],
            &[],
            &checks,
        )
        .unwrap();
        let failed = observed
            .statuses
            .iter()
            .find(|status| status.context.contains("tests"))
            .unwrap();
        assert_eq!(failed.description, "failure: assertion failed in replay");
        assert_eq!(failed.state, "failure");
        assert!(
            observed
                .statuses
                .iter()
                .all(|status| !status.description.contains("old attempt"))
        );
        current["check_run_url"] = Value::Null;
        let unmatched = observe_values(
            &pull(),
            &[run(2, "completed", "failure")],
            &[job("CI required", "success"), current],
            &[],
            &checks,
        )
        .unwrap();
        assert_eq!(
            unmatched
                .statuses
                .iter()
                .find(|status| status.context.contains("tests"))
                .unwrap()
                .description,
            "failure"
        );
    }

    #[test]
    fn source_interface_errors_cannot_reuse_successful_evidence() {
        let completed = run(2, "completed", "success");
        assert!(
            observe_current_run(
                &pull(),
                || Ok(vec![completed.clone()]),
                |_, _| Ok(vec![job("CI required", "success")]),
                || bail!("source statuses unavailable")
            )
            .is_err()
        );
        assert!(
            observe_current_run(
                &pull(),
                || Ok(vec![completed.clone()]),
                |_, _| bail!("jobs unavailable"),
                || Ok((vec![status(1, "external", "success")], Vec::new()))
            )
            .is_err()
        );
        let mut calls = 0;
        assert!(
            observe_current_run(
                &pull(),
                || {
                    calls += 1;
                    if calls == 1 {
                        Ok(vec![completed.clone()])
                    } else {
                        bail!("second page read unavailable")
                    }
                },
                |_, _| Ok(vec![job("CI required", "success")]),
                || Ok((vec![status(1, "external", "success")], Vec::new()))
            )
            .is_err()
        );
    }

    #[test]
    fn another_workflow_changing_during_observation_cannot_leave_old_green() {
        let previous = custom_run(2, 11, "completed", "success");
        let mut retry = previous.clone();
        retry["run_attempt"] = json!(3);
        retry["status"] = json!("in_progress");
        let mut reads = [vec![previous], vec![retry]].into_iter();
        let observed = observe_current_run(
            &pull(),
            || Ok(reads.next().unwrap()),
            |_, _| Ok(Vec::new()),
            || Ok((vec![status(1, "old", "success")], Vec::new())),
        )
        .unwrap();
        assert_eq!(aggregate(&observed), "pending");
        assert_eq!(observed.statuses.len(), 1);
    }
}

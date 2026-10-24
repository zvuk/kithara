use anyhow::{Context, Result, bail};
use serde_json::Value;

use super::projection::{Snapshot, check, job_contexts};
use crate::ci::bridge::{api::Github, model::PullRequest};

pub(super) fn observe(github: &Github, pull: &PullRequest) -> Result<Snapshot> {
    let repository = pull
        .head_repository
        .as_deref()
        .context("PR source repository no longer exists")?;
    observe_current_run(
        pull,
        || github.workflow_runs(repository, &pull.head_sha),
        |run_id, attempt| github.workflow_jobs(repository, run_id, attempt),
    )
}

fn observe_current_run(
    pull: &PullRequest,
    mut read_runs: impl FnMut() -> Result<Vec<Value>>,
    read_jobs: impl FnOnce(u64, u64) -> Result<Vec<Value>>,
) -> Result<Snapshot> {
    let runs = read_runs()?;
    let Some(run) = latest_run(&runs, pull)? else {
        return Ok(pending(
            None,
            "Waiting for CI on the PR source repository and head",
        ));
    };
    let run_id = run["id"]
        .as_u64()
        .context("GitHub workflow run has no id")?;
    let attempt = run["run_attempt"]
        .as_u64()
        .context("GitHub workflow run has no attempt")?;
    let jobs = read_jobs(run_id, attempt)?;
    let current_runs = read_runs()?;
    let current = latest_run(&current_runs, pull)?;
    if current.is_none_or(|current| !same_run(run, current)) {
        return Ok(pending(
            current,
            "Source CI changed while reading jobs; waiting for its current run",
        ));
    }
    snapshot(run, &jobs)
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
    run["head_sha"].as_str() == Some(pull.head_sha.as_str())
        && run["head_branch"].as_str() == Some(pull.head_branch.as_str())
        && run
            .pointer("/head_repository/full_name")
            .and_then(Value::as_str)
            == pull.head_repository.as_deref()
        && run["path"].as_str() == Some(".github/workflows/ci.yml")
        && matches!(run["event"].as_str(), Some("push" | "workflow_dispatch"))
}

fn snapshot(run: &Value, jobs: &[Value]) -> Result<Snapshot> {
    let run_id = run["id"]
        .as_u64()
        .context("GitHub workflow run has no id")?;
    let attempt = run["run_attempt"]
        .as_u64()
        .context("GitHub workflow run has no attempt")?;
    let complete = run["status"].as_str() == Some("completed");
    let url = run["html_url"].as_str();
    let mut statuses = Vec::new();
    for (context, job) in job_contexts("GitHub", jobs)? {
        if job["run_id"].as_u64() != Some(run_id) || job["run_attempt"].as_u64() != Some(attempt) {
            bail!("GitHub job belongs to another workflow run or attempt");
        }
        let (state, detail) = job_state(job)?;
        statuses.push(check(context, state, detail, job["html_url"].as_str()));
    }
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
        json!({ "id": id, "run_attempt": 2, "head_sha": "a".repeat(40), "head_branch": "feature", "head_repository": {"full_name":"fork/kithara"}, "path":".github/workflows/ci.yml", "event":"push", "status":status, "conclusion":conclusion, "html_url":"https://github.com/fork/kithara/actions/runs/2" })
    }

    fn job(name: &str, conclusion: &str) -> Value {
        json!({ "name": name, "run_id": 2, "run_attempt": 2, "status":"completed", "conclusion":conclusion })
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
            aggregate(&snapshot(&completed, &[job("CI required", "success")]).unwrap()),
            "success"
        );
        assert_eq!(aggregate(&snapshot(&completed, &[]).unwrap()), "failure");
        for conclusion in ["failure", "cancelled", "timed_out", "skipped", "neutral"] {
            let snapshot = snapshot(&completed, &[job("CI required", conclusion)]).unwrap();
            assert_eq!(aggregate(&snapshot), "failure", "{conclusion}");
        }
        let failed = run(2, "completed", "failure");
        assert_eq!(
            aggregate(&snapshot(&failed, &[job("CI required", "success")]).unwrap()),
            "failure"
        );
    }

    #[test]
    fn a_rerun_cannot_reuse_an_older_attempt_or_green_jobs_to_finish_early() {
        let running = run(2, "in_progress", "");
        assert_eq!(
            aggregate(&snapshot(&running, &[job("CI required", "success")]).unwrap()),
            "pending"
        );
        let mut previous = job("CI required", "success");
        previous["run_attempt"] = json!(1);
        assert!(snapshot(&running, &[previous]).is_err());
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
        )
        .unwrap();
        assert_eq!(snapshot.statuses[0].state, "failure");
        assert_eq!(snapshot.statuses[1].state, "success");
        assert_eq!(snapshot.statuses[1].description, "skipped");
        assert_eq!(aggregate(&snapshot), "failure");
    }
}

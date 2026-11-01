use anyhow::{Context, Result, bail};
use serde_json::Value;

use super::projection::{Snapshot, check, job_contexts};
use crate::ci::bridge::{
    api::Gitlab,
    model::{CheckStatus, PipelineObservation, Scope, dispatcher, pipeline_observation},
};

struct Pipeline {
    parent: Value,
    bridges: Vec<Value>,
    child: Option<Value>,
}

pub(super) fn observe(gitlab: &Gitlab, pipeline_id: Option<u64>, scope: Scope) -> Result<Snapshot> {
    let Some(id) = pipeline_id else {
        return Ok(pending(None, "Waiting for a GitLab verification pipeline"));
    };
    observe_current_pipeline(
        scope,
        || read_pipeline(gitlab, id, scope),
        |id| gitlab.pipeline_jobs(id),
    )
}

fn read_pipeline(gitlab: &Gitlab, id: u64, scope: Scope) -> Result<Pipeline> {
    let parent = gitlab.pipeline(id)?;
    if parent["id"].as_u64() != Some(id) {
        bail!("GitLab returned another pipeline");
    }
    let mut bridges = gitlab.pipeline_bridges(id)?;
    validate_bridges(&bridges, scope)?;
    let child = match bridges.first_mut() {
        Some(bridge) if !bridge["downstream_pipeline"].is_null() => {
            let child = gitlab.pipeline_child(&bridge["downstream_pipeline"])?;
            if child["sha"].as_str().is_none() || child["sha"] != parent["sha"] {
                bail!("GitLab downstream pipeline belongs to another commit");
            }
            bridge["downstream_pipeline"]["status"] = child["status"].clone();
            Some(child)
        }
        _ => None,
    };
    Ok(Pipeline {
        parent,
        bridges,
        child,
    })
}

fn observe_current_pipeline(
    scope: Scope,
    mut read_pipeline: impl FnMut() -> Result<Pipeline>,
    mut read_jobs: impl FnMut(u64) -> Result<Vec<Value>>,
) -> Result<Snapshot> {
    let initial = read_pipeline()?;
    let parent_id = initial.parent["id"]
        .as_u64()
        .context("GitLab parent pipeline has no id")?;
    let mut statuses = job_checks("GitLab / parent", &read_jobs(parent_id)?)?;
    if let Some(child) = &initial.child {
        let child_id = child["id"]
            .as_u64()
            .context("GitLab child pipeline has no id")?;
        statuses.extend(job_checks("GitLab", &read_jobs(child_id)?)?);
    }
    let current = read_pipeline()?;
    if !same_pipeline(&initial, &current) {
        return Ok(pending(
            current.parent["web_url"].as_str(),
            "GitLab verification changed while reading jobs; waiting for its current run",
        ));
    }
    statuses.extend(job_checks("GitLab", &current.bridges)?);
    let mut snapshot = summary(&current.parent, &current.bridges, scope)?;
    if snapshot.statuses[0].state == "success"
        && statuses.iter().any(|status| status.state == "pending")
    {
        snapshot.statuses[0].state = "pending";
        snapshot.statuses[0].description = "GitLab verification jobs are still active".into();
        snapshot.complete = false;
    }
    snapshot.statuses.extend(statuses);
    Ok(snapshot)
}

fn same_pipeline(previous: &Pipeline, current: &Pipeline) -> bool {
    let pipeline_fields = ["/id", "/sha", "/status"];
    same_fields(&previous.parent, &current.parent, &pipeline_fields)
        && previous.bridges.len() == current.bridges.len()
        && previous
            .bridges
            .iter()
            .zip(&current.bridges)
            .all(|(previous, current)| {
                same_fields(
                    previous,
                    current,
                    &[
                        "/id",
                        "/name",
                        "/status",
                        "/downstream_pipeline/id",
                        "/downstream_pipeline/project_id",
                        "/downstream_pipeline/status",
                    ],
                )
            })
        && match (&previous.child, &current.child) {
            (None, None) => true,
            (Some(previous), Some(current)) => same_fields(previous, current, &pipeline_fields),
            _ => false,
        }
}

fn same_fields(previous: &Value, current: &Value, fields: &[&str]) -> bool {
    fields
        .iter()
        .all(|field| previous.pointer(field) == current.pointer(field))
}

fn pending(url: Option<&str>, detail: &str) -> Snapshot {
    Snapshot {
        statuses: vec![check("GitLab / pipeline", "pending", detail, url)],
        complete: false,
        target_url: url.map(str::to_owned),
    }
}

fn validate_bridges(bridges: &[Value], scope: Scope) -> Result<()> {
    if bridges.len() > 1
        || bridges
            .iter()
            .any(|bridge| bridge["name"].as_str() != Some(&dispatcher(scope.kind())))
    {
        bail!("GitLab verification produced an unexpected downstream dispatcher");
    }
    Ok(())
}

fn summary(parent: &Value, bridges: &[Value], scope: Scope) -> Result<Snapshot> {
    validate_bridges(bridges, scope)?;
    let parent_status = parent["status"]
        .as_str()
        .context("GitLab pipeline has no status")?;
    let children = bridges
        .iter()
        .map(|bridge| {
            let name = bridge["name"]
                .as_str()
                .context("GitLab dispatcher has no name")?;
            Ok((
                name,
                bridge
                    .pointer("/downstream_pipeline/status")
                    .and_then(Value::as_str),
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    let observation = pipeline_observation(scope.kind(), parent_status, &children);
    let (state, detail) = match &observation {
        PipelineObservation::Running => ("pending", "GitLab verification is running"),
        PipelineObservation::Succeeded => ("success", "GitLab verification pipeline succeeded"),
        PipelineObservation::Cancelled => ("failure", "GitLab verification was canceled"),
        PipelineObservation::Failed(_) => ("failure", "GitLab verification pipeline failed"),
        PipelineObservation::Invalid(_) => (
            "error",
            "GitLab verification has no valid downstream pipeline",
        ),
    };
    let url = parent["web_url"].as_str();
    Ok(Snapshot {
        statuses: vec![check("GitLab / pipeline", state, detail, url)],
        complete: !matches!(
            observation,
            PipelineObservation::Running | PipelineObservation::Invalid(_)
        ),
        target_url: url.map(str::to_owned),
    })
}

fn job_checks(host: &str, jobs: &[Value]) -> Result<Vec<CheckStatus>> {
    job_contexts(host, jobs)?
        .into_iter()
        .map(|(context, job)| {
            let status = job["status"].as_str().context("GitLab job has no status")?;
            let state = match status {
                "success" | "skipped" => "success",
                "manual" if job["allow_failure"].as_bool() == Some(true) => "success",
                "failed" | "canceled" => "failure",
                _ => "pending",
            };
            Ok(check(context, state, status, job["web_url"].as_str()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use serde_json::json;

    use super::*;

    fn bridge(status: &str) -> Value {
        json!({"name":"dispatch:quarantine", "status":"running", "downstream_pipeline":{"status":status}})
    }

    fn pipeline(child_id: u64, child_status: &str) -> Pipeline {
        Pipeline {
            parent: json!({"id":1, "sha":"head", "status":"success", "web_url":"https://gitlab.example/pipelines/1"}),
            bridges: vec![
                json!({"id":10, "name":"dispatch:quarantine", "status":"success", "downstream_pipeline":{"id":child_id, "project_id":2747, "status":child_status}}),
            ],
            child: Some(json!({"id":child_id, "sha":"head", "status":child_status})),
        }
    }

    fn success_jobs(id: u64) -> Vec<Value> {
        vec![json!({"name":format!("pipeline-{id}-job"), "status":"success"})]
    }

    #[test]
    fn a_changed_child_or_parent_during_job_listing_cannot_publish_stale_success() {
        let mut parent_changed = pipeline(2, "success");
        parent_changed.parent["status"] = json!("running");
        let mut dispatcher_changed = pipeline(2, "success");
        dispatcher_changed.bridges[0]["id"] = json!(11);
        let mut child_disappeared = pipeline(2, "success");
        child_disappeared.child = None;
        child_disappeared.bridges.clear();
        for current in [
            pipeline(3, "success"),
            pipeline(2, "running"),
            parent_changed,
            dispatcher_changed,
            child_disappeared,
        ] {
            let reads = RefCell::new(Vec::new());
            let mut listed = [pipeline(2, "success"), current].into_iter();
            let observed = observe_current_pipeline(
                Scope::Full,
                || {
                    reads.borrow_mut().push("pipeline".to_owned());
                    Ok(listed.next().expect("exactly two pipeline reads"))
                },
                |id| {
                    reads.borrow_mut().push(format!("jobs:{id}"));
                    Ok(success_jobs(id))
                },
            )
            .unwrap();
            assert_eq!(
                *reads.borrow(),
                ["pipeline", "jobs:1", "jobs:2", "pipeline"]
            );
            assert_eq!(observed.statuses.len(), 1);
            assert_eq!(observed.statuses[0].state, "pending");
            assert!(!observed.complete);
        }
    }

    #[test]
    fn active_jobs_block_success_while_advisory_failures_remain_individually_red() {
        for (job_status, expected_summary, expected_job) in [
            ("pending", "pending", "pending"),
            ("running", "pending", "pending"),
            ("success", "success", "success"),
            ("failed", "success", "failure"),
        ] {
            let reads = RefCell::new(Vec::new());
            let observed = observe_current_pipeline(
                Scope::Full,
                || {
                    reads.borrow_mut().push("pipeline".to_owned());
                    Ok(pipeline(2, "success"))
                },
                |id| {
                    reads.borrow_mut().push(format!("jobs:{id}"));
                    Ok(if id == 2 {
                        vec![
                            json!({"name":"apple:test", "status":job_status, "allow_failure":true}),
                        ]
                    } else {
                        success_jobs(id)
                    })
                },
            )
            .unwrap();
            assert_eq!(
                *reads.borrow(),
                ["pipeline", "jobs:1", "jobs:2", "pipeline"]
            );
            assert_eq!(observed.statuses[0].state, expected_summary, "{job_status}");
            assert_eq!(
                observed.complete,
                expected_summary == "success",
                "{job_status}"
            );
            let job = observed
                .statuses
                .iter()
                .find(|status| status.context.starts_with("GitLab / apple:test ["))
                .unwrap();
            assert_eq!(job.state, expected_job, "{job_status}");
        }
    }

    #[test]
    fn optional_manual_jobs_do_not_hold_a_completed_pipeline_pending() {
        for (allowed, expected) in [(true, "success"), (false, "pending")] {
            let observed = observe_current_pipeline(
                Scope::Full,
                || Ok(pipeline(2, "success")),
                |id| {
                    Ok(if id == 2 {
                        vec![json!({
                            "name": "deep:rtsan",
                            "status": "manual",
                            "allow_failure": allowed,
                        })]
                    } else {
                        success_jobs(id)
                    })
                },
            )
            .unwrap();
            assert_eq!(observed.statuses[0].state, expected);
            let manual = observed
                .statuses
                .iter()
                .find(|status| status.context.starts_with("GitLab / deep:rtsan ["))
                .unwrap();
            assert_eq!(manual.state, expected);
            assert_eq!(manual.description, "manual");
        }
    }

    #[test]
    fn running_parent_still_exposes_failed_child_jobs_and_advisory_failure_stays_red() {
        let parent = json!({"status":"running"});
        let running = summary(&parent, &[bridge("running")], Scope::Full).unwrap();
        assert_eq!(running.statuses[0].state, "pending");
        let jobs = [json!({"name":"apple:test", "status":"failed", "allow_failure":true})];
        assert_eq!(job_checks("GitLab", &jobs).unwrap()[0].state, "failure");
        let succeeded = summary(
            &json!({"status":"success"}),
            &[bridge("success")],
            Scope::Full,
        )
        .unwrap();
        assert_eq!(succeeded.statuses[0].state, "success");
        assert_eq!(job_checks("GitLab", &jobs).unwrap()[0].state, "failure");
    }

    #[test]
    fn the_requested_dispatcher_is_required_and_unrelated_children_are_rejected() {
        let parent = json!({"status":"success"});
        let mut unrelated = bridge("success");
        unrelated["name"] = json!("dispatch:main");
        assert!(summary(&parent, &[unrelated], Scope::Full).is_err());
        assert!(
            summary(
                &parent,
                &[bridge("success"), bridge("success")],
                Scope::Full
            )
            .is_err()
        );
        assert_eq!(
            summary(&parent, &[], Scope::Full).unwrap().statuses[0].state,
            "error"
        );
        assert!(summary(&parent, &[bridge("success")], Scope::Draft).is_err());
    }

    #[test]
    fn a_successful_parent_cannot_hide_a_running_failed_or_cancelled_child() {
        let parent = json!({"status":"success"});
        for (child, expected) in [
            ("running", "pending"),
            ("failed", "failure"),
            ("canceled", "failure"),
            ("skipped", "failure"),
        ] {
            assert_eq!(
                summary(&parent, &[bridge(child)], Scope::Full)
                    .unwrap()
                    .statuses[0]
                    .state,
                expected,
                "{child}"
            );
        }
    }
}

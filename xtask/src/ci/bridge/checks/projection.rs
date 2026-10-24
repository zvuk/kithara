use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use serde_json::Value;
use sha2::{Digest as _, Sha256};

use super::{github, gitlab};
use crate::ci::bridge::{
    api::{Github, Gitlab},
    model::{CheckStatus, PullRequest, Scope},
};

pub(super) struct Snapshot {
    pub(super) statuses: Vec<CheckStatus>,
    pub(super) complete: bool,
    pub(super) target_url: Option<String>,
}

pub(in crate::ci::bridge) fn report_pull(
    github: &Github,
    gitlab: &Gitlab,
    pull: &PullRequest,
    pipeline_id: Option<u64>,
    scope: Scope,
) -> Result<()> {
    let latest = match github.latest_statuses(&pull.head_sha) {
        Ok(latest) => latest,
        Err(error) => {
            let statuses = [
                check(
                    "kithara/github-ci",
                    "error",
                    "Unable to read current PR statuses",
                    None,
                ),
                check(
                    "GitLab / pipeline",
                    "error",
                    "Unable to read current PR statuses",
                    None,
                ),
            ];
            github.report_checks(&pull.head_sha, &statuses, &BTreeMap::new())?;
            return Err(error);
        }
    };
    let sources = [
        (
            "GitHub / ",
            "kithara/github-ci",
            github::observe(github, pull),
        ),
        (
            "GitLab / ",
            "GitLab / pipeline",
            gitlab::observe(gitlab, pipeline_id, scope),
        ),
    ];
    let mut statuses = Vec::new();
    for (prefix, summary, observed) in sources {
        statuses.extend(project_source(prefix, summary, observed, &latest));
    }
    github.report_checks(&pull.head_sha, &statuses, &latest)
}

fn project_source(
    prefix: &str,
    summary: &str,
    observed: Result<Snapshot>,
    latest: &BTreeMap<String, Value>,
) -> Vec<CheckStatus> {
    let mut snapshot = match observed {
        Ok(snapshot) => snapshot,
        Err(error) => {
            tracing::warn!(source = prefix, %error, "reading PR checks failed");
            Snapshot {
                statuses: vec![check(
                    summary,
                    "error",
                    "Unable to read source CI; bridge will retry",
                    None,
                )],
                complete: false,
                target_url: None,
            }
        }
    };
    reset_missing(&mut snapshot, prefix, latest);
    snapshot.statuses
}

pub(in crate::ci::bridge) fn review_size(lines: &Result<u64>) -> CheckStatus {
    lines.as_ref().map_or_else(
        |_| {
            check(
                "kithara/pr-size",
                "error",
                "Unable to measure PR review lines",
                None,
            )
        },
        |lines| {
            check(
                "kithara/pr-size",
                if *lines <= 5_000 {
                    "success"
                } else {
                    "failure"
                },
                format!("{lines}/5000 added review lines; deletions and unchanged moves excluded"),
                None,
            )
        },
    )
}

pub(super) fn check(
    context: impl Into<String>,
    state: &'static str,
    description: impl Into<String>,
    target_url: Option<&str>,
) -> CheckStatus {
    CheckStatus {
        context: context.into(),
        state,
        description: description.into(),
        target_url: target_url.map(str::to_owned),
    }
}

fn reset_missing(snapshot: &mut Snapshot, prefix: &str, latest: &BTreeMap<String, Value>) {
    let current = snapshot
        .statuses
        .iter()
        .map(|status| status.context.clone())
        .collect::<BTreeSet<_>>();
    let (state, description) = if snapshot.complete {
        ("success", "Not scheduled in the current CI run")
    } else {
        (
            "pending",
            "Waiting for the current CI run to schedule this check",
        )
    };
    snapshot.statuses.extend(
        latest
            .keys()
            .filter(|context| context.starts_with(prefix) && !current.contains(*context))
            .map(|context| check(context, state, description, snapshot.target_url.as_deref())),
    );
}

pub(super) fn job_contexts<'a>(host: &str, jobs: &'a [Value]) -> Result<Vec<(String, &'a Value)>> {
    let names = jobs
        .iter()
        .map(|job| job["name"].as_str().context("CI job has no name"))
        .collect::<Result<Vec<_>>>()?;
    let mut counts = BTreeMap::new();
    for name in &names {
        *counts.entry(job_label(name)).or_insert(0_u64) += 1;
    }
    Ok(names
        .into_iter()
        .zip(jobs)
        .map(|(name, job)| {
            let label = job_label(name);
            let context = format!("{host} / {label}");
            let context = if context.chars().count() > 100 || counts[&label] > 1 {
                let digest = Sha256::digest(name.as_bytes());
                format!(
                    "{} [{}]",
                    context.chars().take(80).collect::<String>(),
                    hex::encode(&digest[..8])
                )
            } else {
                context
            };
            (context, job)
        })
        .collect())
}

fn job_label(name: &str) -> String {
    match (name.split_once(" / "), name.rsplit_once(" / ")) {
        (Some((first, _)), Some((_, last))) => format!("{first} / {last}"),
        _ => name.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use anyhow::anyhow;
    use serde_json::json;

    use super::*;

    #[test]
    fn review_limit_counts_5000_as_pass_and_5001_as_failure() {
        assert_eq!(review_size(&Ok(5_000)).state, "success");
        assert_eq!(review_size(&Ok(5_001)).state, "failure");
        assert_eq!(
            review_size(&Err(anyhow!("missing merge base"))).state,
            "error"
        );
        assert_eq!(review_size(&Ok(0)).context, "kithara/pr-size");
    }

    #[test]
    fn a_source_error_replaces_old_green_without_masking_the_other_host() {
        let latest = BTreeMap::from([
            ("GitHub / lint".into(), json!({"state":"success"})),
            ("GitLab / apple:test".into(), json!({"state":"success"})),
        ]);
        for (failed_prefix, failed_summary, healthy_prefix, healthy_summary) in [
            (
                "GitHub / ",
                "kithara/github-ci",
                "GitLab / ",
                "GitLab / pipeline",
            ),
            (
                "GitLab / ",
                "GitLab / pipeline",
                "GitHub / ",
                "kithara/github-ci",
            ),
        ] {
            let failed = project_source(
                failed_prefix,
                failed_summary,
                Err(anyhow!("source unavailable")),
                &latest,
            );
            assert!(
                failed
                    .iter()
                    .any(|status| status.context == failed_summary && status.state == "error")
            );
            assert!(
                failed
                    .iter()
                    .filter(|status| status.context.starts_with(failed_prefix))
                    .all(|status| status.state != "success")
            );
            let healthy = project_source(
                healthy_prefix,
                healthy_summary,
                Ok(Snapshot {
                    statuses: vec![check(healthy_summary, "success", "passed", None)],
                    complete: true,
                    target_url: None,
                }),
                &latest,
            );
            assert!(
                healthy
                    .iter()
                    .any(|status| status.context == healthy_summary && status.state == "success")
            );
        }
    }

    #[test]
    fn missing_checks_are_cleared_when_complete_and_pending_during_reruns_or_errors() {
        let latest = BTreeMap::from([
            ("GitHub / old".into(), json!({"state": "success"})),
            ("GitLab / keep".into(), json!({"state": "failure"})),
        ]);
        let mut snapshot = Snapshot {
            statuses: Vec::new(),
            complete: false,
            target_url: None,
        };
        reset_missing(&mut snapshot, "GitHub / ", &latest);
        assert_eq!(snapshot.statuses.len(), 1);
        assert_eq!(snapshot.statuses[0].state, "pending");
        snapshot.statuses.clear();
        snapshot.complete = true;
        reset_missing(&mut snapshot, "GitHub / ", &latest);
        assert_eq!(snapshot.statuses[0].state, "success");
        assert!(snapshot.statuses[0].description.contains("Not scheduled"));
    }

    #[test]
    fn long_matrix_names_stay_readable_and_collisions_stay_distinct() {
        let jobs = json!([
            {"name": format!("Gate / lane ({}) / linux-lint", "args".repeat(100))},
            {"name": "Gate / lane (one) / tests"},
            {"name": "Gate / lane (two) / tests"},
            {"name": "x".repeat(200)}
        ]);
        let contexts = job_contexts("GitHub", jobs.as_array().unwrap()).unwrap();
        assert_eq!(contexts[0].0, "GitHub / Gate / linux-lint");
        assert_ne!(contexts[1].0, contexts[2].0);
        assert!(
            contexts
                .iter()
                .all(|(context, _)| context.chars().count() <= 100)
        );
    }
}

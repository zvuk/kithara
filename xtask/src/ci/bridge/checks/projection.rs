use std::{
    collections::{BTreeMap, BTreeSet},
    num::NonZeroU64,
};

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

pub(in crate::ci::bridge) fn review_size(lines: &Result<(u64, NonZeroU64)>) -> CheckStatus {
    lines.as_ref().map_or_else(
        |_| {
            check(
                "kithara/pr-size",
                "error",
                "Unable to read review policy or measure PR review lines",
                None,
            )
        },
        |(lines, limit)| {
            check(
                "kithara/pr-size",
                if *lines <= limit.get() {
                    "success"
                } else {
                    "failure"
                },
                format!(
                    "{lines}/{limit} added review lines; deletions and unchanged moves excluded"
                ),
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
    Ok(check_contexts(host, &names).into_iter().zip(jobs).collect())
}

pub(super) fn check_contexts(host: &str, names: &[&str]) -> Vec<String> {
    names
        .iter()
        .map(|name| check_context(host, name, name.as_bytes()))
        .collect()
}

pub(super) fn check_context(host: &str, label: &str, identity: &[u8]) -> String {
    let context = format!("{host} / {}", job_label(label));
    let digest = Sha256::digest(identity);
    format!(
        "{} [{}]",
        context.chars().take(80).collect::<String>(),
        hex::encode(&digest[..8])
    )
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
    fn review_limit_uses_the_configured_boundary() {
        for (lines, limit, state) in [
            (7, 7, "success"),
            (8, 7, "failure"),
            (8, 9, "success"),
            (0, 7, "success"),
            (5000, 5000, "success"),
            (5001, 5000, "failure"),
        ] {
            let status = review_size(&Ok((lines, NonZeroU64::new(limit).unwrap())));
            assert_eq!(status.state, state, "{lines}/{limit}");
            assert!(status.description.starts_with(&format!("{lines}/{limit} ")));
        }
        assert_eq!(
            review_size(&Err(anyhow!("missing merge base"))).state,
            "error"
        );
        assert_eq!(
            review_size(&Ok((0, NonZeroU64::new(7).unwrap()))).context,
            "kithara/pr-size"
        );
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
        assert!(contexts[0].0.starts_with("GitHub / Gate / linux-lint ["));
        assert_ne!(contexts[1].0, contexts[2].0);
        assert!(
            contexts
                .iter()
                .all(|(context, _)| context.chars().count() <= 100)
        );
    }

    #[test]
    fn context_identities_survive_suffix_lookalikes_and_case_insensitive_publication() {
        let long = format!("status: {}", "a".repeat(90));
        let lookalike = format!("status: {} [fd420d32c3430a63]", "a".repeat(63));
        let contexts = check_contexts("GitHub", &[&long, &lookalike, "lint", "Lint"]);
        assert_eq!(
            contexts
                .iter()
                .map(|name| name.to_lowercase())
                .collect::<BTreeSet<_>>()
                .len(),
            4
        );
        assert!(
            contexts
                .iter()
                .all(|context| context.chars().count() <= 100)
        );
    }
}

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use reqwest::Method;
use serde_json::{Value, json};

use super::{super::model::CheckStatus, Github, http::paginated};
use crate::consts;

impl Github {
    pub(in crate::ci::bridge) fn report_status(
        &self,
        sha: &str,
        state: &str,
        description: &str,
        target_url: Option<&str>,
    ) -> Result<()> {
        let body = check_status_body(consts::STATUS_CONTEXT, state, description, target_url);
        let latest = self.latest_statuses(sha)?;
        self.write_status(sha, &body, latest.get(consts::STATUS_CONTEXT))
    }

    pub(in crate::ci::bridge) fn report_check_status(
        &self,
        sha: &str,
        status: &CheckStatus,
    ) -> Result<()> {
        let latest = self.latest_statuses(sha)?;
        self.report_checks(sha, std::slice::from_ref(status), &latest)
    }

    pub(in crate::ci::bridge) fn report_checks(
        &self,
        sha: &str,
        statuses: &[CheckStatus],
        latest: &BTreeMap<String, Value>,
    ) -> Result<()> {
        for status in statuses {
            let body = check_status_body(
                &status.context,
                status.state,
                &status.description,
                status.target_url.as_deref(),
            );
            self.write_status(sha, &body, latest.get(&status.context))?;
        }
        Ok(())
    }

    fn write_status(&self, sha: &str, body: &Value, previous: Option<&Value>) -> Result<()> {
        if previous.is_some_and(|previous| same_status(previous, body)) {
            return Ok(());
        }
        self.request(
            &Method::POST,
            &format!("/repos/{}/statuses/{sha}", self.config.github_repo),
            Some(body),
        )?;
        Ok(())
    }

    pub(in crate::ci::bridge) fn latest_statuses(
        &self,
        sha: &str,
    ) -> Result<BTreeMap<String, Value>> {
        let statuses = paginated(
            |page| {
                self.request(
                    &Method::GET,
                    &format!(
                        "/repos/{}/commits/{sha}/status?per_page=100&page={page}",
                        self.config.github_repo
                    ),
                    None,
                )
            },
            Some("statuses"),
        )?;
        let mut latest = BTreeMap::new();
        for status in statuses {
            let context = status["context"]
                .as_str()
                .context("GitHub commit status has no context")?
                .to_owned();
            latest.entry(context).or_insert(status);
        }
        Ok(latest)
    }
}

fn check_status_body(
    context: &str,
    state: &str,
    description: &str,
    target_url: Option<&str>,
) -> Value {
    let mut body = json!({
        "state": state,
        "context": context,
        "description": status_description(description),
    });
    if let Some(url) = target_url {
        body["target_url"] = json!(url);
    }
    body
}

fn same_status(previous: &Value, current: &Value) -> bool {
    ["context", "state", "description", "target_url"]
        .iter()
        .all(|key| previous[*key] == current[*key])
}

fn status_description(detail: &str) -> String {
    const LIMIT: usize = 140;
    let single_line = detail.split_whitespace().collect::<Vec<_>>().join(" ");
    if single_line.chars().count() <= LIMIT {
        return single_line;
    }
    single_line
        .chars()
        .take(LIMIT - 3)
        .chain("...".chars())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verification_status_body_normalizes_descriptions_and_includes_only_supplied_urls() {
        let url = "https://gitlab.example/owner/repo/-/pipelines/42";
        let body = check_status_body(
            consts::STATUS_CONTEXT,
            "pending",
            "  GitLab\n verification\t running  ",
            Some(url),
        );

        assert_eq!(body["state"], "pending");
        assert_eq!(body["context"], "kithara/gitlab-verification");
        assert_eq!(body["description"], "GitLab verification running");
        assert_eq!(body["target_url"], url);

        let body = check_status_body(consts::STATUS_CONTEXT, "failure", &"path ".repeat(80), None);
        assert_eq!(
            body["description"],
            format!("{}...", "path ".repeat(27) + "pa")
        );
        assert!(!body.as_object().unwrap().contains_key("target_url"));
    }

    #[test]
    fn status_descriptions_fit_github() {
        let described = status_description(&"path ".repeat(80));
        assert_eq!(described.chars().count(), 140);
        assert!(described.ends_with("..."));
    }

    #[test]
    fn status_reporting_compares_only_the_latest_verdict_description_and_destination() {
        let current = check_status_body(
            "GitHub / lint",
            "failure",
            "failed",
            Some("https://github.com/job/2"),
        );
        let mut previous = current.clone();
        previous["id"] = json!(123);
        previous["created_at"] = json!("yesterday");
        assert!(same_status(&previous, &current));
        for (field, changed) in [
            ("state", "pending"),
            ("description", "running"),
            ("target_url", "https://github.com/job/3"),
        ] {
            let mut previous = previous.clone();
            previous[field] = json!(changed);
            assert!(!same_status(&previous, &current), "{field}");
        }
        let without_url = check_status_body("GitHub / lint", "failure", "failed", None);
        assert!(!same_status(&previous, &without_url));
    }
}

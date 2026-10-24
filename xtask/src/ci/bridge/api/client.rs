use std::fs;

use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use reqwest::{Method, Url};
use serde_json::Value;

use super::{
    super::{
        command::BridgeConfig,
        model::{PipelineObservation, PullRequest, Scope, pipeline_observation},
    },
    http::{Api, Payload},
    sources::pull_request,
};
use crate::ci::run::PipelineKind;

pub(in crate::ci::bridge) struct Github {
    pub(super) config: BridgeConfig,
    api: Api,
    token: String,
}

impl Github {
    pub(in crate::ci::bridge) fn new(config: &BridgeConfig) -> Result<Self> {
        let token = read_secret(&config.github_token_file, "GitHub token")?;
        Ok(Self {
            config: config.clone(),
            api: Api::new()?,
            token,
        })
    }

    pub(in crate::ci::bridge) fn head(&self) -> Result<String> {
        let response = self.request(
            &Method::GET,
            &format!(
                "/repos/{}/git/ref/heads/{}",
                self.config.github_repo, self.config.github_branch
            ),
            None,
        )?;
        string_field(&response, &["object", "sha"], "GitHub branch SHA")
    }

    pub(in crate::ci::bridge) fn merged_pull_request(&self, sha: &str) -> Result<Option<u64>> {
        let response = self.request(
            &Method::GET,
            &format!("/repos/{}/commits/{sha}/pulls", self.config.github_repo),
            None,
        )?;
        let pulls = response
            .as_array()
            .context("GitHub commit pulls response was not an array")?;
        Ok(merged_pull_number(pulls, sha, &self.config.github_branch))
    }

    pub(in crate::ci::bridge) fn open_pull_requests(&self) -> Result<Vec<PullRequest>> {
        const PAGE_SIZE: usize = 100;
        let mut page = 1;
        let mut result = Vec::new();
        loop {
            let response = self.request(
                &Method::GET,
                &format!(
                    "/repos/{}/pulls?state=open&base={}&per_page={PAGE_SIZE}&page={page}",
                    self.config.github_repo, self.config.github_branch
                ),
                None,
            )?;
            let pulls = response
                .as_array()
                .context("GitHub pull list response was not an array")?;
            for pull in pulls {
                let branch = pull
                    .pointer("/base/ref")
                    .and_then(Value::as_str)
                    .context("GitHub pull response has no base branch")?;
                if branch != self.config.github_branch {
                    continue;
                }
                result.push(pull_request(pull)?);
            }
            if pulls.len() < PAGE_SIZE {
                return Ok(result);
            }
            page += 1;
        }
    }

    pub(in crate::ci::bridge) fn git_header(&self) -> String {
        let basic = STANDARD.encode(format!("x-access-token:{}", self.token));
        format!("Authorization: Basic {basic}")
    }

    pub(super) fn request(
        &self,
        method: &Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value> {
        let url = Url::parse(&format!("https://api.github.com{path}"))
            .with_context(|| format!("building GitHub API URL for {path}"))?;
        let payload = body.map(Payload::Json);
        self.api.request(
            method,
            &url,
            ("Authorization", &format!("Bearer {}", self.token)),
            payload.as_ref(),
        )
    }
}

pub(in crate::ci::bridge) struct Gitlab {
    pub(super) config: BridgeConfig,
    api: Api,
    token: String,
}

impl Gitlab {
    pub(in crate::ci::bridge) fn new(config: &BridgeConfig) -> Result<Self> {
        let token = read_secret(&config.gitlab_token_file, "GitLab token")?;
        Ok(Self {
            config: config.clone(),
            api: Api::new()?,
            token,
        })
    }

    pub(in crate::ci::bridge) fn head(&self) -> Result<String> {
        let response = self.request(
            &Method::GET,
            &format!("repository/branches/{}", self.config.gitlab_branch),
            None,
        )?;
        string_field(&response, &["commit", "id"], "GitLab branch SHA")
    }

    pub(in crate::ci::bridge) fn create_pipeline(
        &self,
        reference: &str,
        head_sha: &str,
        base_sha: &str,
        scope: Scope,
    ) -> Result<u64> {
        let form = pipeline_form(reference, head_sha, base_sha, scope);
        let payload = Payload::Form(&form);
        let response = self.request(&Method::POST, "pipeline", Some(&payload))?;
        response["id"]
            .as_u64()
            .context("GitLab pipeline response has no numeric id")
    }

    pub(in crate::ci::bridge) fn verification_pipelines(
        &self,
        reference: &str,
        head_sha: &str,
        base_sha: &str,
    ) -> Result<Vec<u64>> {
        let mut url = self.project_url("pipelines")?;
        url.query_pairs_mut()
            .append_pair("ref", reference)
            .append_pair("per_page", "2");
        let response =
            self.api
                .request(&Method::GET, &url, ("PRIVATE-TOKEN", &self.token), None)?;
        let pipelines = response
            .as_array()
            .context("GitLab pipeline list response was not an array")?;
        let mut ids = Vec::with_capacity(pipelines.len());
        for pipeline in pipelines {
            let pipeline_ref = pipeline["ref"]
                .as_str()
                .context("GitLab pipeline list entry has no ref")?;
            if pipeline_ref != reference {
                bail!(
                    "GitLab pipeline query for {reference} returned unexpected ref {pipeline_ref}"
                );
            }
            let id = pipeline["id"]
                .as_u64()
                .context("GitLab pipeline list entry has no numeric id")?;
            let variables =
                self.request(&Method::GET, &format!("pipelines/{id}/variables"), None)?;
            verification_variables(&variables, head_sha, base_sha)?;
            ids.push(id);
        }
        Ok(ids)
    }

    pub(in crate::ci::bridge) fn default_branch_pipeline(&self, sha: &str) -> Result<Option<u64>> {
        let reference = &self.config.gitlab_branch;
        let mut url = self.project_url("pipelines")?;
        url.query_pairs_mut()
            .append_pair("ref", reference)
            .append_pair("sha", sha)
            .append_pair("source", "push")
            .append_pair("order_by", "id")
            .append_pair("sort", "desc")
            .append_pair("per_page", "1");
        let response =
            self.api
                .request(&Method::GET, &url, ("PRIVATE-TOKEN", &self.token), None)?;
        let pipelines = response
            .as_array()
            .context("GitLab pipeline list response was not an array")?;
        let Some(pipeline) = pipelines.first() else {
            return Ok(None);
        };
        let pipeline_ref = pipeline["ref"]
            .as_str()
            .context("GitLab pipeline list entry has no ref")?;
        let pipeline_sha = pipeline["sha"]
            .as_str()
            .context("GitLab pipeline list entry has no sha")?;
        if pipeline_ref != reference || pipeline_sha != sha {
            bail!(
                "GitLab pipeline query for {reference} at {sha} returned unexpected ref \
                 {pipeline_ref} at {pipeline_sha}"
            );
        }
        Ok(Some(
            pipeline["id"]
                .as_u64()
                .context("GitLab pipeline list entry has no numeric id")?,
        ))
    }

    /// Stop every run still holding a slot on `reference`.
    ///
    /// Deleting the branch does not stop its pipeline: a queued run keeps its
    /// place in the resource group after the ref it names is gone, and on a
    /// runner that takes one job at a time that slot is the whole cost. A
    /// finished pipeline refuses the cancel, so only the unfinished are asked.
    pub(in crate::ci::bridge) fn cancel_pipelines(&self, reference: &str) -> Result<()> {
        let mut url = self.project_url("pipelines")?;
        url.query_pairs_mut().append_pair("ref", reference);
        let response =
            self.api
                .request(&Method::GET, &url, ("PRIVATE-TOKEN", &self.token), None)?;
        let pipelines = response
            .as_array()
            .context("GitLab pipeline list response was not an array")?;
        for pipeline in pipelines {
            let status = pipeline["status"]
                .as_str()
                .context("GitLab pipeline list entry has no status")?;
            if finished(status) {
                continue;
            }
            let id = pipeline["id"]
                .as_u64()
                .context("GitLab pipeline list entry has no numeric id")?;
            self.request(&Method::POST, &format!("pipelines/{id}/cancel"), None)?;
        }
        Ok(())
    }

    pub(in crate::ci::bridge) fn pipeline_observation(
        &self,
        pipeline_id: u64,
        kind: PipelineKind,
    ) -> Result<PipelineObservation> {
        let parent = self.request(&Method::GET, &format!("pipelines/{pipeline_id}"), None)?;
        let parent_status = parent["status"]
            .as_str()
            .context("GitLab pipeline response has no status")?;
        if !finished(parent_status) {
            return Ok(PipelineObservation::Running);
        }
        if parent_status != "success" {
            return Ok(pipeline_observation(kind, parent_status, &[]));
        }
        let bridges = self.pipeline_bridges(pipeline_id)?;
        let children = bridges
            .iter()
            .map(|bridge| {
                (
                    bridge["name"].as_str().unwrap_or(""),
                    bridge
                        .pointer("/downstream_pipeline/status")
                        .and_then(Value::as_str),
                )
            })
            .collect::<Vec<_>>();
        Ok(pipeline_observation(kind, parent_status, &children))
    }

    pub(in crate::ci::bridge) fn ensure_issue(&self, title: &str, description: &str) -> Result<()> {
        let mut url = self.project_url("issues")?;
        url.query_pairs_mut()
            .append_pair("state", "opened")
            .append_pair("search", title);
        let issues = self
            .api
            .request(&Method::GET, &url, ("PRIVATE-TOKEN", &self.token), None)?;
        if issues
            .as_array()
            .into_iter()
            .flatten()
            .any(|issue| issue["title"].as_str() == Some(title))
        {
            return Ok(());
        }
        let form = vec![
            ("title".into(), title.into()),
            ("description".into(), description.into()),
            ("labels".into(), "ci-sync,incident".into()),
        ];
        let payload = Payload::Form(&form);
        self.request(&Method::POST, "issues", Some(&payload))?;
        Ok(())
    }

    pub(in crate::ci::bridge) fn git_header(&self) -> String {
        let basic = STANDARD.encode(format!("{}:{}", self.config.gitlab_username, self.token));
        format!("Authorization: Basic {basic}")
    }

    pub(super) fn request(
        &self,
        method: &Method,
        path: &str,
        payload: Option<&Payload<'_>>,
    ) -> Result<Value> {
        let url = self.project_url(path)?;
        self.api
            .request(method, &url, ("PRIVATE-TOKEN", &self.token), payload)
    }

    fn project_url(&self, path: &str) -> Result<Url> {
        Url::parse(&format!(
            "{}/api/v4/projects/{}/{}",
            self.config.gitlab_origin(),
            self.config.gitlab_project_id,
            path.trim_start_matches('/')
        ))
        .with_context(|| format!("building GitLab project API URL for {path}"))
    }
}

/// The form that starts a verification run. A draft says so, and the parent
/// pipeline dispatches it as a branch.
fn pipeline_form(
    reference: &str,
    head_sha: &str,
    base_sha: &str,
    scope: Scope,
) -> Vec<(String, String)> {
    let mut form = vec![
        ("ref".into(), reference.into()),
        ("variables[][key]".into(), "KITHARA_QUARANTINE_SHA".into()),
        ("variables[][value]".into(), head_sha.into()),
        (
            "variables[][key]".into(),
            "KITHARA_QUARANTINE_BASE_SHA".into(),
        ),
        ("variables[][value]".into(), base_sha.into()),
    ];
    if scope == Scope::Draft {
        form.push(("variables[][key]".into(), "KITHARA_QUARANTINE_DRAFT".into()));
        form.push(("variables[][value]".into(), "1".into()));
    }
    form
}

fn read_secret(path: &std::path::Path, label: &str) -> Result<String> {
    let secret = fs::read_to_string(path)
        .with_context(|| format!("reading {label} {}", path.display()))?
        .trim()
        .to_string();
    if secret.is_empty() {
        bail!("{label} file is empty");
    }
    Ok(secret)
}

fn merged_pull_number(pulls: &[Value], sha: &str, branch: &str) -> Option<u64> {
    pulls.iter().find_map(|pull| {
        let exact_commit = pull["merge_commit_sha"].as_str() == Some(sha);
        let merged = pull.get("merged_at").is_some_and(|value| !value.is_null());
        let exact_branch = pull.pointer("/base/ref").and_then(Value::as_str) == Some(branch);
        (exact_commit && merged && exact_branch)
            .then(|| pull.get("number").and_then(Value::as_u64))
            .flatten()
    })
}

/// Whether a pipeline has reached a state it will not leave.
///
/// A pipeline outside this set is still holding its place in the resource
/// group, which is what both the observation and the cancel ask about.
fn finished(status: &str) -> bool {
    matches!(
        status,
        "success" | "failed" | "canceled" | "skipped" | "manual"
    )
}

fn verification_variables(value: &Value, head_sha: &str, base_sha: &str) -> Result<()> {
    let variables = value
        .as_array()
        .context("GitLab pipeline variables response was not an array")?;
    require_variable(variables, "KITHARA_QUARANTINE_SHA", head_sha)?;
    require_variable(variables, "KITHARA_QUARANTINE_BASE_SHA", base_sha)
}

fn require_variable(variables: &[Value], key: &str, expected: &str) -> Result<()> {
    let values = variables
        .iter()
        .filter(|variable| variable["key"].as_str() == Some(key))
        .map(|variable| variable["value"].as_str())
        .collect::<Vec<_>>();
    match values.as_slice() {
        [Some(value)] if *value == expected => Ok(()),
        [Some(value)] => {
            bail!("GitLab pipeline variable {key} is {value:?}, expected {expected:?}")
        }
        [None] => bail!("GitLab pipeline variable {key} has no string value"),
        _ => bail!(
            "GitLab pipeline must have exactly one {key} variable; observed {}",
            values.len()
        ),
    }
}

pub(super) fn string_field(value: &Value, path: &[&str], label: &str) -> Result<String> {
    let mut current = value;
    for part in path {
        current = current
            .get(part)
            .with_context(|| format!("{label} response is missing {part}"))?;
    }
    current
        .as_str()
        .map(str::to_string)
        .with_context(|| format!("{label} is not a string"))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn merged_provenance_requires_the_exact_commit_and_base() {
        let expected = "0123456789abcdef0123456789abcdef01234567";
        let pulls = json!([
            {
                "number": 1,
                "merged_at": "2026-08-13T00:00:00Z",
                "merge_commit_sha": "89abcdef0123456789abcdef0123456789abcdef",
                "base": {"ref": "main"}
            },
            {
                "number": 2,
                "merged_at": "2026-08-13T00:00:00Z",
                "merge_commit_sha": expected,
                "base": {"ref": "other"}
            },
            {
                "number": 3,
                "merged_at": "2026-08-13T00:00:00Z",
                "merge_commit_sha": expected,
                "base": {"ref": "main"}
            }
        ]);

        assert_eq!(
            merged_pull_number(pulls.as_array().unwrap(), expected, "main"),
            Some(3)
        );
    }

    #[test]
    fn pull_head_is_taken_from_the_api_response_exactly() {
        let head = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(
            pull_request(&json!({
                "number": 427,
                "draft": false,
                "head": {"sha": head, "ref": "feature", "repo": {"full_name": "octocat/kithara"}},
                "user": {"login": "octocat"}
            }))
            .unwrap(),
            PullRequest {
                number: 427,
                head_sha: head.into(),
                head_repository: Some("octocat/kithara".into()),
                head_branch: "feature".into(),
                scope: Scope::Full,
                author: "octocat".into(),
            }
        );
    }

    #[test]
    fn a_draft_pull_request_is_verified_as_a_draft() {
        let pull = pull_request(&json!({
            "number": 427,
            "draft": true,
            "head": {"sha": "0123456789abcdef0123456789abcdef01234567", "ref": "feature"},
            "user": {"login": "octocat"}
        }))
        .unwrap();

        assert_eq!(pull.scope, Scope::Draft);
        assert_eq!(pull.head_repository, None);
        assert!(
            pull_request(&json!({
                "number": 427,
                "head": {"sha": "0123456789abcdef0123456789abcdef01234567", "ref": "feature"},
                "user": {"login": "octocat"}
            }))
            .is_err()
        );
    }

    #[test]
    fn a_deleted_fork_does_not_stop_other_pull_requests_from_being_observed() {
        let pull = pull_request(&json!({
            "number": 427,
            "draft": false,
            "head": {"sha": "a".repeat(40), "ref": "feature", "repo": null},
            "user": {"login": "octocat"}
        }))
        .unwrap();
        assert_eq!(pull.head_repository, None);
        assert_eq!(pull.head_branch, "feature");
    }

    #[test]
    fn a_draft_run_is_marked_for_the_dispatcher() {
        assert_eq!(
            pipeline_form("ref", "head", "base", Scope::Draft)
                .iter()
                .filter(|(key, _)| key == "variables[][key]")
                .map(|(_, value)| value.as_str())
                .collect::<Vec<_>>(),
            [
                "KITHARA_QUARANTINE_SHA",
                "KITHARA_QUARANTINE_BASE_SHA",
                "KITHARA_QUARANTINE_DRAFT"
            ]
        );
        assert!(
            !pipeline_form("ref", "head", "base", Scope::Full)
                .iter()
                .any(|(_, value)| value == "KITHARA_QUARANTINE_DRAFT")
        );
    }

    #[test]
    fn recovery_requires_exact_head_and_base_variables() {
        let variables = json!([
            {"key": "KITHARA_QUARANTINE_SHA", "value": "head"},
            {"key": "KITHARA_QUARANTINE_BASE_SHA", "value": "base"}
        ]);
        verification_variables(&variables, "head", "base").unwrap();
        assert!(verification_variables(&variables, "other", "base").is_err());
        assert!(verification_variables(&json!([]), "head", "base").is_err());
    }
}

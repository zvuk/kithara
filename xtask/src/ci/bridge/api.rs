use std::{fs, time::Duration};

use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use reqwest::{
    Method, Url,
    blocking::{Client, RequestBuilder},
};
use serde_json::{Value, json};

use super::{
    command::BridgeConfig,
    model::{PipelineObservation, PullRequest, pipeline_observation},
};
use crate::consts;

enum Payload<'a> {
    Json(&'a Value),
    Form(&'a [(String, String)]),
}

struct Api {
    client: Client,
}

impl Api {
    fn new() -> Result<Self> {
        let builder = Client::builder()
            .timeout(Duration::from_secs(60))
            .user_agent("kithara-git-bridge/1");
        Ok(Self {
            client: builder.build().context("building bridge HTTP client")?,
        })
    }

    fn request(
        &self,
        method: &Method,
        url: &Url,
        authorization: (&str, &str),
        payload: Option<&Payload<'_>>,
    ) -> Result<Value> {
        let mut request = self
            .client
            .request(method.clone(), url.clone())
            .header(authorization.0, authorization.1);
        request = match payload {
            Some(Payload::Json(body)) => request.json(body),
            Some(Payload::Form(form)) => request.form(form),
            None => request,
        };
        response_json(request, method, url)
    }
}

fn response_json(request: RequestBuilder, method: &Method, url: &Url) -> Result<Value> {
    let response = request
        .send()
        .with_context(|| format!("{method} {url} failed"))?;
    let status = response.status();
    let body = response
        .text()
        .with_context(|| format!("reading {method} {url} response"))?;
    if !status.is_success() {
        let detail = body.chars().take(4_096).collect::<String>();
        bail!("{method} {url} failed with HTTP {status}: {detail}");
    }
    if body.trim().is_empty() {
        Ok(Value::Null)
    } else {
        serde_json::from_str(&body).with_context(|| format!("parsing {method} {url} response"))
    }
}

pub(super) struct Github {
    config: BridgeConfig,
    api: Api,
    token: String,
}

impl Github {
    pub(super) fn new(config: &BridgeConfig) -> Result<Self> {
        let token = read_secret(&config.github_token_file, "GitHub token")?;
        Ok(Self {
            config: config.clone(),
            api: Api::new()?,
            token,
        })
    }

    pub(super) fn head(&self) -> Result<String> {
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

    pub(super) fn merged_pull_request(&self, sha: &str) -> Result<Option<u64>> {
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

    pub(super) fn open_pull_requests(&self) -> Result<Vec<PullRequest>> {
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

    pub(super) fn report_status(&self, sha: &str, state: &str, description: &str) -> Result<()> {
        self.request(
            &Method::POST,
            &format!("/repos/{}/statuses/{sha}", self.config.github_repo),
            Some(&json!({
                "state": state,
                "context": consts::STATUS_CONTEXT,
                "description": status_description(description),
            })),
        )?;
        Ok(())
    }

    pub(super) fn git_header(&self) -> String {
        let basic = STANDARD.encode(format!("x-access-token:{}", self.token));
        format!("Authorization: Basic {basic}")
    }

    fn request(&self, method: &Method, path: &str, body: Option<&Value>) -> Result<Value> {
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

pub(super) struct Gitlab {
    config: BridgeConfig,
    api: Api,
    token: String,
}

impl Gitlab {
    pub(super) fn new(config: &BridgeConfig) -> Result<Self> {
        let token = read_secret(&config.gitlab_token_file, "GitLab token")?;
        Ok(Self {
            config: config.clone(),
            api: Api::new()?,
            token,
        })
    }

    pub(super) fn head(&self) -> Result<String> {
        let response = self.request(
            &Method::GET,
            &format!("repository/branches/{}", self.config.gitlab_branch),
            None,
        )?;
        string_field(&response, &["commit", "id"], "GitLab branch SHA")
    }

    pub(super) fn create_pipeline(
        &self,
        reference: &str,
        head_sha: &str,
        base_sha: &str,
    ) -> Result<u64> {
        let form = vec![
            ("ref".into(), reference.into()),
            ("variables[][key]".into(), "KITHARA_QUARANTINE_SHA".into()),
            ("variables[][value]".into(), head_sha.into()),
            (
                "variables[][key]".into(),
                "KITHARA_QUARANTINE_BASE_SHA".into(),
            ),
            ("variables[][value]".into(), base_sha.into()),
        ];
        let payload = Payload::Form(&form);
        let response = self.request(&Method::POST, "pipeline", Some(&payload))?;
        response["id"]
            .as_u64()
            .context("GitLab pipeline response has no numeric id")
    }

    pub(super) fn verification_pipelines(
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

    /// Stop every run still holding a slot on `reference`.
    ///
    /// Deleting the branch does not stop its pipeline: a queued run keeps its
    /// place in the resource group after the ref it names is gone, and on a
    /// runner that takes one job at a time that slot is the whole cost. A
    /// finished pipeline refuses the cancel, so only the unfinished are asked.
    pub(super) fn cancel_pipelines(&self, reference: &str) -> Result<()> {
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

    pub(super) fn pipeline_observation(&self, pipeline_id: u64) -> Result<PipelineObservation> {
        let parent = self.request(&Method::GET, &format!("pipelines/{pipeline_id}"), None)?;
        let parent_status = parent["status"]
            .as_str()
            .context("GitLab pipeline response has no status")?;
        if !finished(parent_status) {
            return Ok(PipelineObservation::Running);
        }
        if parent_status != "success" {
            return Ok(pipeline_observation(parent_status, &[]));
        }
        let bridges = self.request(
            &Method::GET,
            &format!("pipelines/{pipeline_id}/bridges"),
            None,
        )?;
        let bridges = bridges
            .as_array()
            .context("GitLab bridges response was not an array")?;
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
        Ok(pipeline_observation(parent_status, &children))
    }

    pub(super) fn ensure_issue(&self, title: &str, description: &str) -> Result<()> {
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

    pub(super) fn git_header(&self) -> String {
        let basic = STANDARD.encode(format!("{}:{}", self.config.gitlab_username, self.token));
        format!("Authorization: Basic {basic}")
    }

    fn request(&self, method: &Method, path: &str, payload: Option<&Payload<'_>>) -> Result<Value> {
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

fn status_description(detail: &str) -> String {
    const LIMIT: usize = 140;
    let single_line = detail.split_whitespace().collect::<Vec<_>>().join(" ");
    if single_line.chars().count() <= LIMIT {
        return single_line;
    }
    single_line
        .chars()
        .take(LIMIT - 1)
        .chain(std::iter::once('…'))
        .collect()
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

fn pull_request(value: &Value) -> Result<PullRequest> {
    let number = value["number"]
        .as_u64()
        .context("GitHub pull response has no numeric number")?;
    let head_sha = string_field(value, &["head", "sha"], "GitHub pull head SHA")?;
    let author = string_field(value, &["user", "login"], "GitHub pull author login")?;
    Ok(PullRequest {
        number,
        head_sha,
        author,
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

fn string_field(value: &Value, path: &[&str], label: &str) -> Result<String> {
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
    use super::*;

    #[test]
    fn verification_status_uses_the_required_context_and_exact_head() {
        let sha = "0123456789abcdef0123456789abcdef01234567";
        let path = format!("/repos/owner/repo/statuses/{sha}");
        let body = json!({
            "state": "pending",
            "context": consts::STATUS_CONTEXT,
            "description": status_description("GitLab verification running"),
        });

        assert_eq!(path, format!("/repos/owner/repo/statuses/{sha}"));
        assert_eq!(body["context"], "kithara/gitlab-verification");
    }

    #[test]
    fn status_descriptions_fit_github() {
        let described = status_description(&"path ".repeat(80));
        assert_eq!(described.chars().count(), 140);
        assert!(described.ends_with('…'));
    }

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
            pull_request(
                &json!({"number": 427, "head": {"sha": head}, "user": {"login": "octocat"}})
            )
            .unwrap(),
            PullRequest {
                number: 427,
                head_sha: head.into(),
                author: "octocat".into(),
            }
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

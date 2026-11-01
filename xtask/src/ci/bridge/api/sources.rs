use anyhow::{Context, Result, bail};
use reqwest::Method;
use serde_json::Value;

use super::{
    super::model::{PullRequest, Scope, simple_repository, validate_sha},
    Github, Gitlab,
    client::string_field,
    http::paginated,
};

impl Github {
    pub(in crate::ci::bridge) fn workflow_runs(
        &self,
        repository: &str,
        sha: &str,
    ) -> Result<Vec<Value>> {
        self.source_entries(
            repository,
            sha,
            &format!("actions/runs?head_sha={sha}&per_page=100"),
            Some("workflow_runs"),
        )
    }

    pub(in crate::ci::bridge) fn source_statuses(
        &self,
        repository: &str,
        sha: &str,
    ) -> Result<Vec<Value>> {
        self.source_entries(
            repository,
            sha,
            &format!("commits/{sha}/statuses?per_page=100"),
            None,
        )
    }

    pub(in crate::ci::bridge) fn source_check_runs(
        &self,
        repository: &str,
        sha: &str,
    ) -> Result<Vec<Value>> {
        self.source_entries(
            repository,
            sha,
            &format!("commits/{sha}/check-runs?filter=all&per_page=100"),
            Some("check_runs"),
        )
    }

    fn source_entries(
        &self,
        repository: &str,
        sha: &str,
        path: &str,
        field: Option<&str>,
    ) -> Result<Vec<Value>> {
        if !simple_repository(repository) || !validate_sha(sha) {
            bail!("invalid GitHub source repository or head SHA");
        }
        paginated(
            |page| {
                self.request(
                    &Method::GET,
                    &format!("/repos/{repository}/{path}&page={page}"),
                    None,
                )
            },
            field,
        )
    }

    pub(in crate::ci::bridge) fn workflow_jobs(
        &self,
        repository: &str,
        run_id: u64,
        attempt: u64,
    ) -> Result<Vec<Value>> {
        if !simple_repository(repository) {
            bail!("invalid GitHub workflow repository");
        }
        paginated(
            |page| {
                self.request(
                    &Method::GET,
                    &format!(
                        "/repos/{repository}/actions/runs/{run_id}/attempts/{attempt}/jobs?per_page=100&page={page}"
                    ),
                    None,
                )
            },
            Some("jobs"),
        )
    }
}

impl Gitlab {
    pub(in crate::ci::bridge) fn pipeline(&self, pipeline_id: u64) -> Result<Value> {
        self.request(&Method::GET, &format!("pipelines/{pipeline_id}"), None)
    }

    pub(in crate::ci::bridge) fn pipeline_child(&self, downstream: &Value) -> Result<Value> {
        if downstream["project_id"].as_u64() != Some(self.config.gitlab_project_id) {
            bail!("GitLab downstream pipeline belongs to another project");
        }
        let id = downstream["id"]
            .as_u64()
            .context("GitLab downstream pipeline has no id")?;
        let child = self.pipeline(id)?;
        if child["id"].as_u64() != Some(id) {
            bail!("GitLab returned another downstream pipeline");
        }
        Ok(child)
    }

    pub(in crate::ci::bridge) fn pipeline_jobs(&self, pipeline_id: u64) -> Result<Vec<Value>> {
        self.pipeline_entries(pipeline_id, "jobs?include_retried=false")
    }

    pub(in crate::ci::bridge) fn pipeline_bridges(&self, pipeline_id: u64) -> Result<Vec<Value>> {
        self.pipeline_entries(pipeline_id, "bridges?")
    }

    fn pipeline_entries(&self, pipeline_id: u64, entries: &str) -> Result<Vec<Value>> {
        paginated(
            |page| {
                self.request(
                    &Method::GET,
                    &format!("pipelines/{pipeline_id}/{entries}&per_page=100&page={page}"),
                    None,
                )
            },
            None,
        )
    }
}

pub(super) fn pull_request(value: &Value) -> Result<PullRequest> {
    let number = value["number"]
        .as_u64()
        .context("GitHub pull response has no numeric number")?;
    let head_sha = string_field(value, &["head", "sha"], "GitHub pull head SHA")?;
    let head_branch = string_field(value, &["head", "ref"], "GitHub pull head branch")?;
    let head_repository = value
        .pointer("/head/repo/full_name")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let author = string_field(value, &["user", "login"], "GitHub pull author login")?;
    let draft = value["draft"]
        .as_bool()
        .context("GitHub pull response has no draft flag")?;
    Ok(PullRequest {
        number,
        head_sha,
        head_repository,
        head_branch,
        scope: if draft { Scope::Draft } else { Scope::Full },
        author,
    })
}

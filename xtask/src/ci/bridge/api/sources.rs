use anyhow::{Context, Result, bail};
use reqwest::Method;
use serde_json::Value;

use super::{
    super::model::{PullRequest, Scope, simple_repository, validate_sha},
    Github, Gitlab,
    client::string_field,
    http::paginated,
};
use crate::consts;

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
            &format!("commits/{sha}/check-runs?filter=latest&per_page=100"),
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

    /// The jobs that turned a finished verification red, as [`red_jobs`]
    /// reads them from its downstream pipeline.
    pub(in crate::ci::bridge) fn failed_jobs(&self, pipeline_id: u64) -> Result<Vec<String>> {
        let mut jobs = Vec::new();
        for bridge in self.pipeline_bridges(pipeline_id)? {
            if bridge["downstream_pipeline"].is_null() {
                continue;
            }
            let child = self.pipeline_child(&bridge["downstream_pipeline"])?;
            let id = child["id"]
                .as_u64()
                .context("GitLab child pipeline has no id")?;
            jobs.extend(self.pipeline_jobs(id)?);
        }
        red_jobs(&jobs)
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

/// Every failed job that gates and, when the verdict is one of them, every
/// judged lane that failed beside it: the verdict read their reports, and a
/// lane it did not hold against the change is red only on its own.
fn red_jobs(jobs: &[Value]) -> Result<Vec<String>> {
    let mut gating = Vec::new();
    let mut judged = Vec::new();
    for job in jobs {
        if job["status"].as_str() != Some("failed") {
            continue;
        }
        let name = job["name"]
            .as_str()
            .context("GitLab job has no name")?
            .to_owned();
        if job["allow_failure"].as_bool() == Some(true) {
            judged.push(name);
        } else {
            gating.push(name);
        }
    }
    if gating.iter().any(|name| name == consts::VERDICT_JOB) {
        gating.extend(judged);
    }
    gating.sort();
    gating.dedup();
    Ok(gating)
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
    let tests = xtask::ci::github::pr_tests_block(value["body"].as_str().unwrap_or_default())
        .map_err(|error| error.to_string());
    Ok(PullRequest {
        number,
        head_sha,
        head_repository,
        head_branch,
        scope: if draft { Scope::Draft } else { Scope::Full },
        author,
        tests,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn job(name: &str, status: &str, allow_failure: bool) -> Value {
        json!({"name": name, "status": status, "allow_failure": allow_failure})
    }

    #[test]
    fn a_judged_lane_is_red_only_through_a_failed_verdict() {
        let lanes = [
            job("apple:test", "failed", true),
            job("linux:lint", "failed", false),
            job("apple:lint", "success", false),
        ];
        assert_eq!(red_jobs(&lanes).unwrap(), ["linux:lint"]);

        let judged = [lanes.as_slice(), &[job("verdict", "failed", false)]].concat();
        assert_eq!(
            red_jobs(&judged).unwrap(),
            ["apple:test", "linux:lint", "verdict"]
        );
    }
}

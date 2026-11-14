use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use url::Url;

use super::{
    super::{Deployment, Request},
    encoded, gh, git, output,
};
use crate::Ctx;

struct Gitlab {
    host: String,
    project: String,
}

impl Gitlab {
    fn new(config: &Deployment) -> Self {
        Self {
            host: config.gitlab_host.clone(),
            project: format!("projects/{}", encoded(&config.gitlab_project)),
        }
    }

    fn api(&self, ctx: &Ctx, suffix: &str, body: Option<&Value>) -> Result<Value> {
        let path = format!("{}{suffix}", self.project);
        let args = self.arguments(&path, body.is_some());
        Ok(serde_json::from_str(&output(ctx, "glab", &args, body)?)?)
    }

    fn arguments<'a>(&'a self, path: &'a str, post: bool) -> Vec<&'a str> {
        let mut args = vec!["api", "--hostname", &self.host, path];
        if post {
            args.extend([
                "--method",
                "POST",
                "--header",
                "Content-Type: application/json",
                "--input",
                "-",
            ]);
        }
        args
    }
}

enum Controller {
    DefaultBranch(String),
    ManualJob(u64),
}

impl Controller {
    fn submission(self, variables: &[Value]) -> (String, Value) {
        match self {
            Self::DefaultBranch(reference) => (
                "/pipeline".into(),
                json!({"ref":reference,"variables":variables}),
            ),
            Self::ManualJob(job) => (
                format!("/jobs/{job}/play"),
                json!({"job_variables_attributes":variables}),
            ),
        }
    }
}

fn controller(ctx: &Ctx, gitlab: &Gitlab, repository: &str) -> Result<Controller> {
    let sha = published_controller(|arguments| git(ctx, arguments))?;
    let project = gitlab.api(ctx, "", None)?;
    let default = project["default_branch"]
        .as_str()
        .context("GitLab default branch")?;
    let branch = gitlab.api(
        ctx,
        &format!("/repository/branches/{}", encoded(default)),
        None,
    )?;
    let default_sha = branch["commit"]["id"]
        .as_str()
        .context("GitLab default branch commit")?;
    if default_sha == sha {
        return Ok(Controller::DefaultBranch(default.into()));
    }
    let base = gitlab.api(
        ctx,
        &format!("/repository/merge_base?refs[]={sha}&refs[]={default_sha}"),
        None,
    )?;
    if base["id"].as_str() == Some(&sha) {
        return Ok(Controller::DefaultBranch(default.into()));
    }
    let statuses = gh(
        ctx,
        &format!("repos/{repository}/commits/{sha}/status?per_page=100"),
    )?;
    let target = statuses["statuses"]
        .as_array()
        .context("GitHub statuses")?
        .iter()
        .find(|status| status["context"] == "kithara/gitlab-verification")
        .and_then(|status| status["target_url"].as_str())
        .context("published controller has no native verification pipeline yet")?;
    let url = Url::parse(target)?;
    ensure!(
        url.host_str() == Some(&gitlab.host),
        "native pipeline host mismatch"
    );
    let id = url
        .path_segments()
        .and_then(Iterator::last)
        .context("native pipeline id")?;
    ensure!(
        id.bytes().all(|byte| byte.is_ascii_digit()),
        "invalid native pipeline id"
    );
    let variables = gitlab.api(ctx, &format!("/pipelines/{id}/variables"), None)?;
    ensure!(variables.as_array().context("native variables")?.iter().any(|variable| variable["key"] == "KITHARA_QUARANTINE_SHA" && variable["value"] == sha), "native controller pipeline does not verify the local commit");
    let bridges = gitlab.api(ctx, &format!("/pipelines/{id}/bridges?per_page=100"), None)?;
    let children: Vec<_> = bridges
        .as_array()
        .context("native pipeline bridges")?
        .iter()
        .filter_map(|bridge| bridge["downstream_pipeline"]["id"].as_u64())
        .collect();
    ensure!(
        children.len() == 1,
        "native verification needs one existing child pipeline"
    );
    let jobs = gitlab.api(
        ctx,
        &format!(
            "/pipelines/{}/jobs?scope[]=manual&per_page=100",
            children[0]
        ),
        None,
    )?;
    Ok(Controller::ManualJob(manual_job(&jobs)?))
}

fn published_controller(mut query: impl FnMut(&[&str]) -> Result<String>) -> Result<String> {
    let head = query(&["rev-parse", "HEAD"])?;
    let upstream = query(&["rev-parse", "--verify", "@{upstream}"]).context(
        "benchmark controller has no upstream; publish the branch and configure tracking",
    )?;
    println!("CI controller {upstream}");
    if head != upstream {
        println!("Local HEAD {head} differs from published controller {upstream}");
    }
    Ok(upstream)
}

fn manual_job(jobs: &Value) -> Result<u64> {
    let job = jobs
        .as_array()
        .context("native manual jobs")?
        .iter()
        .find(|job| job["name"] == "deep:bench" && job["status"] == "manual")
        .context("native review has no unplayed deep:bench job; use its current pipeline")?;
    ensure!(
        job["allow_failure"] == true,
        "preview benchmark must be an optional manual job"
    );
    job["id"].as_u64().context("native benchmark job id")
}

pub(super) fn run(ctx: &Ctx, config: &Deployment, request: &Request) -> Result<()> {
    let gitlab = Gitlab::new(config);
    let controller = controller(ctx, &gitlab, &config.github_repo)?;
    let mut values: Vec<_> = request
        .parameters()
        .into_iter()
        .map(|(key, value)| json!({"key":key,"value":value}))
        .collect();
    values.push(json!({"key":"RUN_BENCHMARKS","value":"1"}));
    let (path, body) = controller.submission(&values);
    let response = gitlab.api(ctx, &path, Some(&body))?;
    println!(
        "{}",
        response["web_url"]
            .as_str()
            .context("native benchmark URL")?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;

    #[test]
    fn bench_native_json_command_declares_content_type() {
        let client = Gitlab {
            host: "gitlab.example".into(),
            project: "projects/example".into(),
        };
        let mut command = Command::new("glab");
        command.args(client.arguments("projects/example/jobs/17/play", true));
        let recorded: Vec<_> = command
            .get_args()
            .map(|argument| argument.to_str().unwrap())
            .collect();
        assert_eq!(
            recorded,
            [
                "api",
                "--hostname",
                "gitlab.example",
                "projects/example/jobs/17/play",
                "--method",
                "POST",
                "--header",
                "Content-Type: application/json",
                "--input",
                "-"
            ]
        );
    }

    #[test]
    fn bench_controller_uses_upstream_and_rejects_missing_tracking() {
        for upstream in [Some("published"), None] {
            let mut recorded = Vec::new();
            let result = published_controller(|arguments| {
                recorded.push(arguments.join(" "));
                if arguments.contains(&"HEAD") {
                    return Ok("local-ahead".into());
                }
                upstream.map(str::to_owned).context("no tracking branch")
            });
            assert_eq!(
                recorded,
                ["rev-parse HEAD", "rev-parse --verify @{upstream}"]
            );
            match upstream {
                Some(expected) => assert_eq!(result.unwrap(), expected),
                None => assert!(result.unwrap_err().to_string().contains("no upstream")),
            }
        }
    }

    #[test]
    fn bench_preview_plays_one_optional_job_without_another_pipeline() {
        let jobs = json!([{"id":17,"name":"deep:bench","status":"manual","allow_failure":true}]);
        let job = manual_job(&jobs).unwrap();
        let variables = vec![json!({"key":"BENCH_FILTER","value":"group/(a|b)$; $(false)"})];
        let (path, body) = Controller::ManualJob(job).submission(&variables);
        assert_eq!(path, "/jobs/17/play");
        assert_eq!(body, json!({"job_variables_attributes":variables}));
        let (path, body) = Controller::DefaultBranch("develop".into()).submission(&variables);
        assert_eq!(path, "/pipeline");
        assert_eq!(body, json!({"ref":"develop","variables":variables}));
        for (status, allowed) in [("success", true), ("failed", true), ("manual", false)] {
            assert!(
                manual_job(
                    &json!([{"id":17,"name":"deep:bench","status":status,"allow_failure":allowed}])
                )
                .is_err()
            );
        }
    }
}

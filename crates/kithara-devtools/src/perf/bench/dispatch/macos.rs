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

fn controller(ctx: &Ctx, gitlab: &Gitlab, repository: &str, preflight: bool) -> Result<Controller> {
    let sha = published_controller(preflight, |arguments| git(ctx, arguments))?;
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
    let uses_default = if default_sha == sha {
        true
    } else {
        let base = gitlab.api(
            ctx,
            &format!("/repository/merge_base?refs[]={sha}&refs[]={default_sha}"),
            None,
        )?;
        base["id"].as_str() == Some(&sha)
    };
    if uses_default {
        ensure!(!preflight, "native preflight requires a pinned manual job");
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
    Ok(Controller::ManualJob(manual_job(
        &jobs,
        preflight.then_some(&sha),
    )?))
}

fn published_controller(
    preflight: bool,
    mut query: impl FnMut(&[&str]) -> Result<String>,
) -> Result<String> {
    let head = query(&["rev-parse", "HEAD"])?;
    let upstream = query(&["rev-parse", "--verify", "@{upstream}"]).context(
        "benchmark controller has no upstream; publish the branch and configure tracking",
    )?;
    ensure!(
        !preflight || head == upstream,
        "native preflight requires local HEAD to match the published controller"
    );
    println!("CI controller {upstream}");
    if head != upstream {
        println!("Local HEAD {head} differs from published controller {upstream}");
    }
    Ok(upstream)
}

fn manual_job(jobs: &Value, controller: Option<&str>) -> Result<u64> {
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
    if let Some(sha) = controller {
        ensure!(
            job["pipeline"]["sha"].as_str() == Some(sha),
            "native preflight manual job pipeline SHA does not match the published controller"
        );
    }
    job["id"].as_u64().context("native benchmark job id")
}

fn variables(request: &Request, input: Option<&str>) -> Vec<Value> {
    let mut values: Vec<_> = request
        .parameters()
        .into_iter()
        .map(|(key, value)| json!({"key":key,"value":value}))
        .collect();
    values.push(input.map_or_else(
        || json!({"key":"RUN_BENCHMARKS","value":"1"}),
        |raw| json!({"key":"BENCH_PREFLIGHT","value":raw}),
    ));
    values
}

pub(super) fn run(
    ctx: &Ctx,
    config: &Deployment,
    request: &Request,
    input: Option<&str>,
) -> Result<()> {
    let gitlab = Gitlab::new(config);
    let controller = controller(ctx, &gitlab, &config.github_repo, input.is_some())?;
    let (path, body) = controller.submission(&variables(request, input));
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
            let result = published_controller(false, |arguments| {
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
        let job = manual_job(&jobs, None).unwrap();
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
                    &json!([{"id":17,"name":"deep:bench","status":status,"allow_failure":allowed}]),
                    None
                )
                .is_err()
            );
        }
    }

    #[test]
    fn bench_native_ordinary_payload_keeps_source_mode_and_policy_parameters() {
        let request = Request {
            target: "p:b:f".into(),
            filter: "group/(a|b)$; $(false)".into(),
            head: "head".into(),
            base: "base".into(),
            ..Request::default()
        };
        let variables = variables(&request, None);
        let expected = concat!(
            r#"[{"key":"BENCH_TARGET","value":"p:b:f"},"#,
            r#"{"key":"BENCH_FILTER","value":"group/(a|b)$; $(false)"},"#,
            r#"{"key":"BENCH_REF","value":"head"},{"key":"BENCH_BASE_REF","value":"base"},"#,
            r#"{"key":"BENCH_MODE","value":"timing"},{"key":"BENCH_ALLOCATOR_POLICY","value":"system"},"#,
            r#"{"key":"RUN_BENCHMARKS","value":"1"}]"#,
        );
        for (controller, path, body) in [
            (
                Controller::ManualJob(17),
                "/jobs/17/play",
                format!(r#"{{"job_variables_attributes":{expected}}}"#),
            ),
            (
                Controller::DefaultBranch("develop".into()),
                "/pipeline",
                format!(r#"{{"ref":"develop","variables":{expected}}}"#),
            ),
        ] {
            let (actual_path, actual_body) = controller.submission(&variables);
            assert_eq!(actual_path, path);
            assert_eq!(actual_body.to_string(), body);
        }
    }

    #[cfg(unix)]
    #[test]
    fn bench_native_preflight_plays_only_a_pinned_manual_job() {
        use std::{fs, os::unix::fs::PermissionsExt};

        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let manifest = root.join("Cargo.toml");
        fs::write(&manifest, "[workspace]\n").unwrap();
        fs::create_dir(root.join(".config")).unwrap();
        let fake = r#"
#!/bin/sh
set -eu
scenario=$(cat scenario)
case "${0##*/}:${2-}:${4-}" in
    git:HEAD:) test "$scenario" != head-mismatch && echo pinned || echo foreign ;;
    git:--verify:) echo pinned ;;
    gh:repos/owner/repo/commits/pinned/status?per_page=100:)
        echo '{"statuses":[{"context":"kithara/gitlab-verification","target_url":"https://gitlab.example/owner/repo/-/pipelines/9"}]}'
        ;;
    glab:--hostname:projects/1)
        echo '{"default_branch":"develop"}'
        ;;
    glab:--hostname:projects/1/repository/branches/develop)
        if [ "$scenario" = equal-default ]; then
            echo '{"commit":{"id":"pinned"}}'
        else
            echo '{"commit":{"id":"develop"}}'
        fi
        ;;
    glab:--hostname:projects/1/repository/merge_base*)
        if [ "$scenario" = ancestor ]; then
            echo '{"id":"pinned"}'
        else
            echo '{"id":"older"}'
        fi
        ;;
    glab:--hostname:projects/1/pipelines/9/variables)
        echo '[{"key":"KITHARA_QUARANTINE_SHA","value":"pinned"}]'
        ;;
    glab:--hostname:projects/1/pipelines/9/bridges*)
        echo '[{"downstream_pipeline":{"id":10}}]'
        ;;
    glab:--hostname:projects/1/pipelines/10/jobs*)
        case "$scenario" in
            absent) pipeline='{}' ;;
            different) pipeline='{"sha":"foreign"}' ;;
            *) pipeline='{"sha":"pinned"}' ;;
        esac
        printf '[{"id":17,"name":"deep:bench","status":"manual","allow_failure":true,"pipeline":%s}]\n' "$pipeline"
        ;;
    glab:--hostname:projects/1/jobs/17/play)
        cat > submitted.json
        echo '{"web_url":"https://gitlab.example/jobs/17"}'
        ;;
    *) exit 70 ;;
esac
"#;
        let mut tools = String::new();
        for name in ["git", "gh", "glab"] {
            use std::fmt::Write as _;
            let path = root.join(name);
            fs::write(&path, fake.trim_start()).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            writeln!(
                tools,
                "[tools.{name}]\nprogram = {:?}",
                path.to_str().unwrap()
            )
            .unwrap();
        }
        fs::write(root.join(".config/xtask.toml"), tools).unwrap();
        let ctx = Ctx::load_from_manifest(&manifest).unwrap();
        let config = Deployment {
            github_repo: "owner/repo".into(),
            gitlab_host: "gitlab.example".into(),
            gitlab_project: "1".into(),
        };
        let request = Request {
            target: "p:b:f".into(),
            head: "b".repeat(40),
            base: "a".repeat(40),
            ..Request::default()
        };
        let raw = "{\n  \"head\": \"with exact UTF-8: \u{00e9}\"\n}\n";
        let submitted = root.join("submitted.json");
        for (scenario, expected) in [
            (
                "head-mismatch",
                "local HEAD to match the published controller",
            ),
            ("equal-default", "requires a pinned manual job"),
            ("ancestor", "requires a pinned manual job"),
            ("different", "manual job pipeline SHA does not match"),
            ("absent", "manual job pipeline SHA does not match"),
        ] {
            fs::write(root.join("scenario"), scenario).unwrap();
            let actual = run(&ctx, &config, &request, Some(raw)).unwrap_err();
            assert!(
                actual.to_string().contains(expected),
                "{scenario}: {actual:#}"
            );
            assert!(!submitted.exists(), "{scenario} submitted a request");
        }
        fs::write(root.join("scenario"), "valid").unwrap();
        run(&ctx, &config, &request, Some(raw)).unwrap();
        let payload: Value = serde_json::from_slice(&fs::read(submitted).unwrap()).unwrap();
        assert_eq!(
            payload,
            json!({"job_variables_attributes": [
                {"key":"BENCH_TARGET", "value":"p:b:f"},
                {"key":"BENCH_FILTER", "value":""},
                {"key":"BENCH_REF", "value":request.head},
                {"key":"BENCH_BASE_REF", "value":request.base},
                {"key":"BENCH_MODE", "value":"timing"},
                {"key":"BENCH_ALLOCATOR_POLICY", "value":"system"},
                {"key":"BENCH_PREFLIGHT", "value":raw},
            ]})
        );
    }
}

//! Which CI job this process runs as.

use std::{env, process};

use anyhow::{Context, Result, bail, ensure};
use reqwest::Url;

/// The credential-free Git repository named by the current CI job.
pub(crate) fn source_repository(var: &dyn Fn(&str) -> Option<String>) -> Result<Option<Url>> {
    let value = if let Some(project) = var("CI_PROJECT_URL") {
        format!(
            "{}.git",
            project.trim_end_matches('/').trim_end_matches(".git")
        )
    } else {
        match (var("GITHUB_SERVER_URL"), var("GITHUB_REPOSITORY")) {
            (Some(server), Some(repository)) => {
                format!("{}/{repository}.git", server.trim_end_matches('/'))
            }
            (None, None) => return Ok(None),
            _ => bail!("CI Git source requires GITHUB_SERVER_URL and GITHUB_REPOSITORY"),
        }
    };
    repository_url(&value).map(Some)
}

/// Git credentials stay in the transport environment, outside manifests and evidence.
pub(crate) fn repository_url(value: &str) -> Result<Url> {
    let url = Url::parse(value).context("invalid Git repository URL")?;
    ensure!(
        url.password().is_none()
            && (url.username().is_empty() || url.scheme() == "ssh")
            && url.query().is_none()
            && url.fragment().is_none(),
        "Git repository URL must not contain credentials, a query, or a fragment"
    );
    Ok(url)
}

/// Whether this process runs inside a GitLab CI job.
pub(crate) fn is_gitlab() -> bool {
    gitlab_in(&|name| env::var(name).ok())
}

fn gitlab_in(var: &dyn Fn(&str) -> Option<String>) -> bool {
    var("GITLAB_CI").is_some_and(|value| !value.is_empty())
}

/// Whether this process runs inside a GitHub Actions job.
pub(crate) fn is_github() -> bool {
    github_in(&|name| env::var(name).ok())
}

pub(crate) fn github_in(var: &dyn Fn(&str) -> Option<String>) -> bool {
    var("GITHUB_ACTIONS").is_some_and(|value| value == "true")
}

/// Names this process to a job that waits on a lock it holds: the link to
/// the CI job it runs in, or the local command that took the lock.
///
/// A variable the CI job left unset is named in its place as
/// `<NAME unset>`. The name only serves a waiter's log, and a job never
/// fails for its diagnostics.
pub(crate) fn lock_holder() -> String {
    holder_in(&|name| env::var(name).ok())
}

fn holder_in(var: &dyn Fn(&str) -> Option<String>) -> String {
    let named = |name: &str| {
        var(name)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| format!("<{name} unset>"))
    };
    if github_in(var) {
        return format!(
            "{}/{}/actions/runs/{}/attempts/{} on {}",
            named("GITHUB_SERVER_URL"),
            named("GITHUB_REPOSITORY"),
            named("GITHUB_RUN_ID"),
            named("GITHUB_RUN_ATTEMPT"),
            named("RUNNER_NAME"),
        );
    }
    if gitlab_in(var) {
        return named("CI_JOB_URL");
    }
    format!(
        "pid {} running {}",
        process::id(),
        env::args_os()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(" ")
    )
}

#[cfg(test)]
mod tests {
    use std::process;

    use super::*;

    fn environment<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_owned())
        }
    }

    #[test]
    fn source_repository_uses_job_context_with_blank_cache_flags() {
        for (pairs, expected) in [
            (
                vec![("CI_PROJECT_URL", "https://gitlab.example/group/repo")],
                "https://gitlab.example/group/repo.git",
            ),
            (
                vec![
                    ("GITHUB_ACTIONS", ""),
                    ("GITHUB_SERVER_URL", "https://github.example"),
                    ("GITHUB_REPOSITORY", "owner/repo"),
                ],
                "https://github.example/owner/repo.git",
            ),
        ] {
            assert_eq!(
                source_repository(&environment(&pairs))
                    .unwrap()
                    .unwrap()
                    .as_str(),
                expected
            );
        }
        assert!(source_repository(&environment(&[])).unwrap().is_none());
        assert!(source_repository(&environment(&[("GITHUB_REPOSITORY", "owner/repo")])).is_err());
    }

    #[test]
    fn repository_credentials_never_enter_consumer_evidence() {
        for url in [
            "https://token@github.example/owner/repo.git",
            "ssh://git:secret@github.example/owner/repo.git",
            "https://github.example/owner/repo.git?token=secret",
        ] {
            assert!(repository_url(url).is_err());
        }
        assert!(repository_url("ssh://git@github.example/owner/repo.git").is_ok());
    }

    #[test]
    fn a_github_holder_links_the_run_attempt_and_names_the_runner() {
        let var = environment(&[
            ("GITHUB_ACTIONS", "true"),
            ("GITHUB_SERVER_URL", "https://github.com"),
            ("GITHUB_REPOSITORY", "zvuk/kithara"),
            ("GITHUB_RUN_ID", "77"),
            ("GITHUB_RUN_ATTEMPT", "2"),
            ("RUNNER_NAME", "kithara-3"),
        ]);

        assert_eq!(
            holder_in(&var),
            "https://github.com/zvuk/kithara/actions/runs/77/attempts/2 on kithara-3"
        );
    }

    #[test]
    fn a_gitlab_holder_is_the_job_link() {
        let var = environment(&[
            ("GITLAB_CI", "true"),
            ("CI_JOB_URL", "https://gitlab.example/-/jobs/29"),
        ]);

        assert_eq!(holder_in(&var), "https://gitlab.example/-/jobs/29");
    }

    /// Naming the holder serves a waiter's log: a variable the CI job left
    /// unset is named in its place, and the job that takes the lock works on.
    #[test]
    fn a_holder_names_in_place_what_its_ci_job_left_unset() {
        let github = environment(&[
            ("GITHUB_ACTIONS", "true"),
            ("GITHUB_SERVER_URL", "https://github.com"),
            ("GITHUB_REPOSITORY", "zvuk/kithara"),
            ("GITHUB_RUN_ID", "77"),
            ("GITHUB_RUN_ATTEMPT", "2"),
        ]);
        let gitlab = environment(&[("GITLAB_CI", "true"), ("CI_JOB_URL", "")]);

        assert_eq!(
            holder_in(&github),
            "https://github.com/zvuk/kithara/actions/runs/77/attempts/2 on <RUNNER_NAME unset>"
        );
        assert_eq!(holder_in(&gitlab), "<CI_JOB_URL unset>");
    }

    #[test]
    fn a_local_holder_names_its_process() {
        let holder = holder_in(&environment(&[]));

        assert!(
            holder.starts_with(&format!("pid {} running ", process::id())),
            "{holder}"
        );
    }

    #[test]
    fn gitlab_is_named_by_a_non_empty_flag() {
        assert!(gitlab_in(&environment(&[("GITLAB_CI", "true")])));
        assert!(!gitlab_in(&environment(&[("GITLAB_CI", "")])));
        assert!(!gitlab_in(&environment(&[])));
    }
}

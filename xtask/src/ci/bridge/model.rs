use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Paths that define the trusted `GitLab` judge. Pull-request product content is
/// tested with these paths restored from the synchronized default branch.
///
/// Being listed here is not by itself a reason to reject a pull request. What a
/// change does to the judge decides that, and `control::classify` is where it is
/// decided: an entry added beside the existing ones promises nothing new about
/// the old ones, while an edited or deleted entry can turn a run green without
/// the code earning it.
pub(super) const CONTROL_PATHS: &[&str] = &[
    ".gitlab-ci.yml",
    ".gitlab/",
    ".config/ci-pins.toml",
    ".config/just/",
    ".config/mutation-suites.toml",
    ".config/nextest.toml",
    ".config/xtask.toml",
    "ci/",
    "docker/",
    "justfile",
    "xtask/",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Direction {
    Equal,
    GithubAhead,
    GitlabAhead,
    Diverged,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum VerificationState {
    Testing,
    Verified,
    Rejected,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct PullRequest {
    pub(super) number: u64,
    pub(super) head_sha: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum PipelineObservation {
    Running,
    Succeeded,
    Failed(String),
    Invalid(String),
}

pub(super) fn pipeline_observation(
    parent: &str,
    children: &[(&str, Option<&str>)],
) -> PipelineObservation {
    if !matches!(
        parent,
        "success" | "failed" | "canceled" | "skipped" | "manual"
    ) {
        return PipelineObservation::Running;
    }
    if parent != "success" {
        return PipelineObservation::Failed(parent.to_owned());
    }
    let [(name, Some(child))] = children else {
        return PipelineObservation::Invalid(format!(
            "successful quarantine parent must have exactly one downstream child; observed {}",
            children.len()
        ));
    };
    if *name != "dispatch:quarantine" {
        return PipelineObservation::Invalid(format!(
            "successful quarantine parent produced unexpected child {name:?}"
        ));
    }
    match *child {
        "success" => PipelineObservation::Succeeded,
        "failed" | "canceled" | "skipped" | "manual" => {
            PipelineObservation::Failed((*child).to_owned())
        }
        _ => PipelineObservation::Running,
    }
}

pub(super) fn direction_for(
    github_sha: &str,
    gitlab_sha: &str,
    mut is_ancestor: impl FnMut(&str, &str) -> Result<bool>,
) -> Result<Direction> {
    if github_sha == gitlab_sha {
        return Ok(Direction::Equal);
    }
    if is_ancestor(gitlab_sha, github_sha)? {
        return Ok(Direction::GithubAhead);
    }
    if is_ancestor(github_sha, gitlab_sha)? {
        return Ok(Direction::GitlabAhead);
    }
    Ok(Direction::Diverged)
}

pub(super) fn validate_sha(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub(super) fn simple_branch(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

pub(super) fn simple_repository(value: &str) -> bool {
    let mut parts = value.split('/');
    matches!(
        (parts.next(), parts.next(), parts.next()),
        (Some(owner), Some(name), None)
            if simple_repository_part(owner) && simple_repository_part(name)
    )
}

fn simple_repository_part(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

pub(super) fn regular_file(path: &Path) -> bool {
    path.is_absolute()
        && path
            .symlink_metadata()
            .is_ok_and(|metadata| metadata.is_file() && !metadata.file_type().is_symlink())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directions_are_ancestry_based() {
        let github_ahead = direction_for("github", "gitlab", |older, newer| {
            Ok((older, newer) == ("gitlab", "github"))
        })
        .unwrap();
        assert_eq!(github_ahead, Direction::GithubAhead);

        let gitlab_ahead = direction_for("github", "gitlab", |older, newer| {
            Ok((older, newer) == ("github", "gitlab"))
        })
        .unwrap();
        assert_eq!(gitlab_ahead, Direction::GitlabAhead);

        assert_eq!(
            direction_for("same", "same", |_, _| Ok(false)).unwrap(),
            Direction::Equal
        );
        assert_eq!(
            direction_for("github", "gitlab", |_, _| Ok(false)).unwrap(),
            Direction::Diverged
        );
    }

    #[test]
    fn repository_and_branch_values_are_bounded() {
        assert!(simple_repository("zvuk/kithara"));
        assert!(!simple_repository("zvuk/kithara/extra"));
        assert!(!simple_repository("zvuk/kithara?token=secret"));
        assert!(simple_branch("main"));
        assert!(!simple_branch("heads/main"));
    }

    #[test]
    fn successful_parent_requires_exactly_one_successful_child() {
        assert_eq!(
            pipeline_observation("success", &[("dispatch:quarantine", Some("success"))]),
            PipelineObservation::Succeeded
        );
        assert!(matches!(
            pipeline_observation("success", &[]),
            PipelineObservation::Invalid(_)
        ));
        assert!(matches!(
            pipeline_observation(
                "success",
                &[
                    ("dispatch:quarantine", Some("success")),
                    ("dispatch:quarantine", Some("success")),
                ]
            ),
            PipelineObservation::Invalid(_)
        ));
        assert!(matches!(
            pipeline_observation("success", &[("dispatch:quarantine", None)]),
            PipelineObservation::Invalid(_)
        ));
        assert!(matches!(
            pipeline_observation("success", &[("dispatch:main", Some("success"))]),
            PipelineObservation::Invalid(_)
        ));
    }

    #[test]
    fn running_and_terminal_child_observations_are_distinct() {
        assert_eq!(
            pipeline_observation("running", &[]),
            PipelineObservation::Running
        );
        assert_eq!(
            pipeline_observation("success", &[("dispatch:quarantine", Some("running"))]),
            PipelineObservation::Running
        );
        assert_eq!(
            pipeline_observation("success", &[("dispatch:quarantine", Some("failed"))]),
            PipelineObservation::Failed("failed".into())
        );
    }
}

use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::ci::run::PipelineKind;

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

/// How much of the merge a verification checks.
///
/// A draft is still being written: it runs what its change touched, as a
/// branch push does. A pull request ready for review runs everything.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum Scope {
    #[default]
    Full,
    Draft,
}

impl Scope {
    /// The pipeline kind the scope's run is dispatched as.
    pub(super) const fn kind(self) -> PipelineKind {
        match self {
            Self::Full => PipelineKind::Quarantine,
            Self::Draft => PipelineKind::Branch,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct PullRequest {
    pub(super) number: u64,
    pub(super) head_sha: String,
    pub(super) head_repository: Option<String>,
    pub(super) head_branch: String,
    /// Whether GitHub marks it a draft, which scopes its verification.
    pub(super) scope: Scope,
    /// GitHub login of whoever opened it, matched against the trusted authors
    /// the host configured.
    pub(super) author: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct CheckStatus {
    pub(super) context: String,
    pub(super) state: &'static str,
    pub(super) description: String,
    pub(super) target_url: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum PipelineObservation {
    Running,
    Succeeded,
    /// The run was stopped before it could judge anything.
    ///
    /// Carried apart from `Failed` because it is not a verdict: a queue emptied
    /// by hand, an auto-cancel, or a runner taken down says only that the
    /// branch has not been verified yet.
    Cancelled,
    Failed(String),
    Invalid(String),
}

/// The parent names each dispatcher `dispatch:<kind>` for the `KITHARA_PIPELINE_KIND`
/// it sets. The judge accepts only the kind it started or expects, so a run of
/// another kind can never pass for it.
pub(super) fn dispatcher(kind: PipelineKind) -> String {
    format!("dispatch:{}", kind.name())
}

pub(super) fn pipeline_observation(
    kind: PipelineKind,
    parent: &str,
    children: &[(&str, Option<&str>)],
) -> PipelineObservation {
    if !matches!(
        parent,
        "success" | "failed" | "canceled" | "skipped" | "manual"
    ) {
        return PipelineObservation::Running;
    }
    if parent == "canceled" {
        return PipelineObservation::Cancelled;
    }
    if parent != "success" {
        return PipelineObservation::Failed(parent.to_owned());
    }
    let [(name, Some(child))] = children else {
        return PipelineObservation::Invalid(format!(
            "successful {} parent must have exactly one downstream child; observed {}",
            kind.name(),
            children.len()
        ));
    };
    if *name != dispatcher(kind) {
        return PipelineObservation::Invalid(format!(
            "successful {} parent produced unexpected child {name:?}",
            kind.name()
        ));
    }
    match *child {
        "success" => PipelineObservation::Succeeded,
        "canceled" => PipelineObservation::Cancelled,
        "failed" | "skipped" | "manual" => PipelineObservation::Failed((*child).to_owned()),
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

/// The default branch on each side of the bridge.
///
/// The two are separate values because the repositories disagree on the name.
/// Crossing them is silent rather than loud: GitHub answers an unknown base
/// with an empty pull list, so a swap would leave the bridge idling.
#[derive(Clone, Copy, Debug)]
pub(super) struct Branches<'a> {
    pub(super) github: &'a str,
    pub(super) gitlab: &'a str,
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
    use std::fs;

    use clap::ValueEnum;

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
            pipeline_observation(
                PipelineKind::Quarantine,
                "success",
                &[("dispatch:quarantine", Some("success"))]
            ),
            PipelineObservation::Succeeded
        );
        assert!(matches!(
            pipeline_observation(PipelineKind::Quarantine, "success", &[]),
            PipelineObservation::Invalid(_)
        ));
        assert!(matches!(
            pipeline_observation(
                PipelineKind::Quarantine,
                "success",
                &[
                    ("dispatch:quarantine", Some("success")),
                    ("dispatch:quarantine", Some("success")),
                ]
            ),
            PipelineObservation::Invalid(_)
        ));
        assert!(matches!(
            pipeline_observation(
                PipelineKind::Quarantine,
                "success",
                &[("dispatch:quarantine", None)]
            ),
            PipelineObservation::Invalid(_)
        ));
        assert!(matches!(
            pipeline_observation(
                PipelineKind::Quarantine,
                "success",
                &[("dispatch:main", Some("success"))]
            ),
            PipelineObservation::Invalid(_)
        ));
    }

    /// A run someone stopped reports nothing about the branch. Reading it as a
    /// failure marked six pull requests rejected for a queue that was emptied
    /// underneath them, so the cancellation is carried apart from a verdict —
    /// and the parent and the child are stopped by the same hands.
    #[test]
    fn a_cancellation_is_carried_apart_from_a_failure() {
        assert_eq!(
            pipeline_observation(PipelineKind::Quarantine, "canceled", &[]),
            PipelineObservation::Cancelled
        );
        assert_eq!(
            pipeline_observation(
                PipelineKind::Quarantine,
                "success",
                &[("dispatch:quarantine", Some("canceled"))]
            ),
            PipelineObservation::Cancelled
        );
        assert_eq!(
            pipeline_observation(PipelineKind::Quarantine, "failed", &[]),
            PipelineObservation::Failed("failed".into())
        );
    }

    #[test]
    fn running_and_terminal_child_observations_are_distinct() {
        assert_eq!(
            pipeline_observation(PipelineKind::Quarantine, "running", &[]),
            PipelineObservation::Running
        );
        assert_eq!(
            pipeline_observation(
                PipelineKind::Quarantine,
                "success",
                &[("dispatch:quarantine", Some("running"))]
            ),
            PipelineObservation::Running
        );
        assert_eq!(
            pipeline_observation(
                PipelineKind::Quarantine,
                "success",
                &[("dispatch:quarantine", Some("failed"))]
            ),
            PipelineObservation::Failed("failed".into())
        );
    }

    #[test]
    fn a_successful_parent_is_judged_by_the_dispatcher_of_its_own_kind() {
        let main = dispatcher(PipelineKind::Main);
        let quarantine = dispatcher(PipelineKind::Quarantine);

        assert_eq!(
            pipeline_observation(PipelineKind::Main, "success", &[(&main, Some("success"))]),
            PipelineObservation::Succeeded
        );
        assert!(matches!(
            pipeline_observation(
                PipelineKind::Main,
                "success",
                &[(&quarantine, Some("success"))]
            ),
            PipelineObservation::Invalid(_)
        ));
        assert_eq!(
            pipeline_observation(PipelineKind::Main, "success", &[(&main, Some("failed"))]),
            PipelineObservation::Failed("failed".into())
        );
    }

    #[test]
    fn every_pipeline_kind_has_its_dispatcher_in_the_parent_pipeline() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("xtask has a workspace root");
        let pipeline = fs::read_to_string(root.join(".gitlab-ci.yml"))
            .expect("the parent pipeline definition is readable");

        for &kind in PipelineKind::value_variants() {
            let job = format!("{}:", dispatcher(kind));
            let mut lines = pipeline.lines();
            lines.find(|line| *line == job).unwrap_or_else(|| {
                panic!("the parent pipeline defines the {} dispatcher", kind.name())
            });
            let mut block = lines.take_while(|line| {
                line.is_empty()
                    || line.starts_with('#')
                    || line.starts_with(' ')
                    || line.starts_with('\t')
            });

            assert!(
                block.any(|line| {
                    line.trim_start().strip_prefix("KITHARA_PIPELINE_KIND: ") == Some(kind.name())
                }),
                "{job} sets KITHARA_PIPELINE_KIND to {}",
                kind.name()
            );
        }
    }
}

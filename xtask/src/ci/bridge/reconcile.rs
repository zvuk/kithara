use std::{
    fs::{self, File, OpenOptions},
    path::Path,
};

use anyhow::{Context, Result, bail};
use kithara_devtools::lock::FileLock;
use tracing::{info, warn};

use super::{
    api::{Github, Gitlab},
    command::BridgeConfig,
    git::{GitRepo, Judged},
    ledger::{Ledger, LedgerEntry},
    model::{
        Direction, PipelineObservation, PullRequest, VerificationState, direction_for, validate_sha,
    },
};

pub(super) struct Bridge {
    config: BridgeConfig,
    github: Github,
    gitlab: Gitlab,
    repo: GitRepo,
}

struct ReconcileLock {
    _lock: FileLock,
}

impl ReconcileLock {
    fn acquire(state_dir: &Path) -> Result<Self> {
        let file = open_reconcile_lock(state_dir)?;
        let lock = FileLock::exclusive(file)
            .with_context(|| format!("locking bridge state {}", state_dir.display()))?;
        Ok(Self { _lock: lock })
    }

    #[cfg(test)]
    fn try_acquire(state_dir: &Path) -> Result<Option<Self>> {
        let file = open_reconcile_lock(state_dir)?;
        match FileLock::try_exclusive(file) {
            Ok(lock) => Ok(Some(Self { _lock: lock })),
            Err(fs4::TryLockError::WouldBlock) => Ok(None),
            Err(fs4::TryLockError::Error(error)) => {
                Err(error).with_context(|| format!("locking bridge state {}", state_dir.display()))
            }
        }
    }
}

fn open_reconcile_lock(state_dir: &Path) -> Result<File> {
    fs::create_dir_all(state_dir)
        .with_context(|| format!("creating bridge state {}", state_dir.display()))?;
    let path = state_dir.join("reconcile.lock");
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
        .with_context(|| format!("opening bridge lock {}", path.display()))
}

impl Bridge {
    pub(super) fn new(config: BridgeConfig) -> Result<Self> {
        fs::create_dir_all(&config.state_dir)
            .with_context(|| format!("creating bridge state {}", config.state_dir.display()))?;
        Ok(Self {
            github: Github::new(&config)?,
            gitlab: Gitlab::new(&config)?,
            repo: GitRepo::new(&config.state_dir, &config)?,
            config,
        })
    }

    pub(super) fn reconcile_once(&self) -> Result<()> {
        let _lock = ReconcileLock::acquire(&self.config.state_dir)?;
        reconcile_main_first(
            || self.reconcile_main(),
            |base_sha| self.verify_open_pulls(base_sha),
        )
    }

    fn reconcile_main(&self) -> Result<Option<String>> {
        self.repo.fetch(&self.github, &self.gitlab)?;
        let github_sha = self.github.head()?;
        let gitlab_sha = self.gitlab.head()?;
        require_sha("GitHub", &github_sha)?;
        require_sha("GitLab", &gitlab_sha)?;
        let direction = direction_for(&github_sha, &gitlab_sha, |older, newer| {
            self.repo.is_ancestor(older, newer)
        })?;
        info!(?direction, %github_sha, %gitlab_sha, "repository direction observed");

        match direction {
            Direction::Equal => Ok(Some(github_sha)),
            Direction::GitlabAhead => {
                self.export_gitlab(&gitlab_sha)?;
                Ok(None)
            }
            Direction::GithubAhead => {
                self.import_github(&github_sha, &gitlab_sha)?;
                Ok(None)
            }
            Direction::Diverged => {
                let detail = format!(
                    "GitHub `{github_sha}` and GitLab `{gitlab_sha}` are not ancestors of each \
                     other. Synchronization stopped."
                );
                self.gitlab
                    .ensure_issue("GitHub and GitLab main branches diverged", &detail)?;
                bail!("GitHub and GitLab histories diverged");
            }
        }
    }

    /// Immediately. Waiting for the default branch's own pipeline gated nothing:
    /// a red one does not un-merge the commit, so the wait only delayed a
    /// decision already taken. What it did buy was an hour in which both sides
    /// could move, and two sides that have both moved cannot be reconciled by a
    /// fast-forward — the one failure this bridge cannot repair.
    ///
    /// `GitLab` changes are judged before merge. GitHub changes are imported only
    /// when the exact head belongs to a merged pull request; trying to judge one
    /// after merge cannot protect either branch and can only split them.
    fn export_gitlab(&self, gitlab_sha: &str) -> Result<()> {
        self.repo.push_github(&self.github, gitlab_sha)
    }

    fn import_github(&self, github_sha: &str, gitlab_base_sha: &str) -> Result<()> {
        fast_forward_github_import(
            github_sha,
            gitlab_base_sha,
            &self.config.branch,
            |sha| self.github.merged_pull_request(sha),
            || {
                self.repo.fetch(&self.github, &self.gitlab)?;
                Ok((self.github.head()?, self.gitlab.head()?))
            },
            |detail| {
                self.gitlab
                    .ensure_issue("Untrusted direct GitHub main update", detail)
            },
            |sha, branch| self.repo.push_gitlab(&self.gitlab, sha, branch),
        )
    }

    fn verify_open_pulls(&self, base_sha: &str) -> Result<()> {
        let ledger = Ledger::new(&self.config.state_dir)?;
        for pull in self.github.open_pull_requests()? {
            if let Err(error) = self.verify_pull(&ledger, &pull, base_sha) {
                warn!(
                    pull_number = pull.number,
                    head_sha = %pull.head_sha,
                    %base_sha,
                    %error,
                    "GitLab verification tick failed without changing its verdict"
                );
            }
        }
        if let Err(error) = sweep_quarantine_refs(
            base_sha,
            || self.repo.gitlab_quarantine_refs(&self.gitlab),
            |refs| self.repo.delete_gitlab(&self.gitlab, refs),
        ) {
            warn!(
                %base_sha,
                %error,
                "verification branches left behind by a moved base were not removed"
            );
        }
        Ok(())
    }

    fn verify_pull(&self, ledger: &Ledger, pull: &PullRequest, base_sha: &str) -> Result<()> {
        require_sha("GitHub pull request", &pull.head_sha)?;
        self.repo
            .fetch_pull_head(&self.github, pull.number, &pull.head_sha)?;
        // A trusted author answers for the CI configuration the same way they
        // answer for the code, so their pull request is never rejected for
        // touching it. For everyone else the rule stands, and a contributor who
        // cannot open a GitLab merge request is not the one it is aimed at —
        // see the follow-up in `model`.
        let trusted = self
            .config
            .trusted_authors
            .iter()
            .any(|author| author == &pull.author);
        let changed_controls = if trusted {
            Vec::new()
        } else {
            self.repo
                .weakening_control_paths(base_sha, &pull.head_sha)?
        };
        let entry = ledger.reserve(&pull.head_sha, base_sha)?;
        if reject_control_changes(
            pull.number,
            &pull.head_sha,
            &entry,
            &changed_controls,
            |sha, state, detail| self.github.report_status(sha, state, detail),
            |attempt, detail| ledger.reject(&pull.head_sha, base_sha, attempt, detail),
        )? {
            return Ok(());
        }
        if entry.state != VerificationState::Testing {
            return Ok(());
        }

        let Some(pipeline_id) = entry.pipeline_id else {
            let reference = quarantine_ref(&pull.head_sha, base_sha, entry.attempt);
            // The base is merged in either way; trust decides only whether the
            // pull request keeps its own CI configuration on top of it.
            let judged = match self
                .repo
                .judged_commit(base_sha, &pull.head_sha, !trusted)?
            {
                Judged::Commit(judged) => judged,
                Judged::Conflict => {
                    return reject_unmergeable(
                        pull.number,
                        &pull.head_sha,
                        base_sha,
                        &entry,
                        |sha, state, detail| self.github.report_status(sha, state, detail),
                        |attempt, detail| ledger.reject(&pull.head_sha, base_sha, attempt, detail),
                    );
                }
            };
            self.repo.push_gitlab(&self.gitlab, &judged, &reference)?;
            let discovered =
                self.gitlab
                    .verification_pipelines(&reference, &pull.head_sha, base_sha)?;
            start_verification(
                &pull.head_sha,
                entry.attempt,
                &discovered,
                || {
                    self.gitlab
                        .create_pipeline(&reference, &pull.head_sha, base_sha)
                },
                |attempt, pipeline_id| {
                    ledger.attach(&pull.head_sha, base_sha, attempt, pipeline_id)
                },
                |sha, state, detail| self.github.report_status(sha, state, detail),
                |attempt, pipeline_id| {
                    ledger.announce(&pull.head_sha, base_sha, attempt, pipeline_id)
                },
            )?;
            return Ok(());
        };

        if !entry.announced {
            self.github
                .report_status(&pull.head_sha, "pending", "GitLab verification running")?;
            ledger.announce(&pull.head_sha, base_sha, entry.attempt, pipeline_id)?;
            return Ok(());
        }

        observe_verification(
            &pull.head_sha,
            pipeline_id,
            |id| self.gitlab.pipeline_observation(id),
            |sha, state, detail| self.github.report_status(sha, state, detail),
            |id, state, detail| {
                ledger.finish(&pull.head_sha, base_sha, entry.attempt, id, state, detail)
            },
        )
    }
}

fn reconcile_main_first(
    reconcile_main: impl FnOnce() -> Result<Option<String>>,
    verify_pulls: impl FnOnce(&str) -> Result<()>,
) -> Result<()> {
    if let Some(base) = reconcile_main()?
        && let Err(error) = verify_pulls(&base)
    {
        warn!(%error, %base, "pull-request verification tick failed");
    }
    Ok(())
}

/// Branch a verification runs on.
///
/// Abbreviated, because this name is read by people in `GitLab`'s own interface,
/// and two full shas in it came to 101 characters — enough to break the layout
/// of every list the branch appears in. The pair is not what identifies the run
/// anyway: the pipeline carries `KITHARA_QUARANTINE_HEAD_SHA` and
/// `KITHARA_QUARANTINE_BASE_SHA` in full, and `verification_pipelines` refuses
/// any pipeline whose variables disagree. The name only has to address one
/// branch, and the rule that starts these runs matches the `quarantine/`
/// prefix alone.
fn quarantine_ref(head_sha: &str, base_sha: &str, attempt: u64) -> String {
    format!(
        "quarantine/gh/{}-{}/attempt-{attempt}",
        abbreviate(head_sha),
        abbreviate(base_sha)
    )
}

/// Twelve hex digits, the width git itself grows to on a repository this size.
/// Shorter reads better and collides sooner; a collision here would have to
/// land between two pull-request heads verified against the same base.
fn abbreviate(sha: &str) -> &str {
    &sha[..sha.len().min(12)]
}

/// The verification branches nothing will ever name again.
///
/// A quarantine ref is addressed by the pair it was judged for, so the moment
/// main moves the bridge writes a new name and the old branch is left behind.
/// Matching on the base alone covers every naming scheme the bridge has used,
/// because each one spells the base out abbreviated or in full.
fn superseded_quarantine_refs<'a>(refs: &'a [String], base_sha: &str) -> Vec<&'a str> {
    let base = abbreviate(base_sha);
    refs.iter()
        .map(String::as_str)
        .filter(|reference| reference.starts_with("quarantine/") && !reference.contains(base))
        .collect()
}

/// Drop the verification branches a moved base left behind.
///
/// The bridge pushes one of these per attempt and never reads it back: the
/// verdict lives in the ledger, and `verify_pull` reserves against whatever
/// main is now, so a branch judged for an earlier base is orphaned the moment
/// main moves. Nothing addresses it again and nothing reads its pipeline, so
/// what is left is exhaust - 197 of them had piled up on `GitLab` by the time
/// anyone counted. Cancelling an orphan's pipeline on the way out is a gain on
/// a runner that takes one job at a time.
fn sweep_quarantine_refs(
    base_sha: &str,
    list: impl FnOnce() -> Result<Vec<String>>,
    delete: impl FnOnce(&[&str]) -> Result<()>,
) -> Result<()> {
    let listed = list()?;
    let superseded = superseded_quarantine_refs(&listed, base_sha);
    if superseded.is_empty() {
        return Ok(());
    }
    delete(&superseded)?;
    info!(
        dropped = superseded.len(),
        %base_sha,
        "verification branches left behind by a moved base removed"
    );
    Ok(())
}

fn resolve_pipeline(pipeline_ids: &[u64]) -> Result<Option<u64>> {
    match pipeline_ids {
        [] => Ok(None),
        [pipeline_id] => Ok(Some(*pipeline_id)),
        _ => bail!("multiple pipelines exist for one exact verification attempt: {pipeline_ids:?}"),
    }
}

fn recover_or_create(pipeline_ids: &[u64], create: impl FnOnce() -> Result<u64>) -> Result<u64> {
    resolve_pipeline(pipeline_ids)?.map_or_else(create, Ok)
}

fn start_verification(
    head_sha: &str,
    attempt: u64,
    pipeline_ids: &[u64],
    create: impl FnOnce() -> Result<u64>,
    attach: impl FnOnce(u64, u64) -> Result<()>,
    report: impl FnOnce(&str, &str, &str) -> Result<()>,
    announce: impl FnOnce(u64, u64) -> Result<()>,
) -> Result<()> {
    let pipeline_id = recover_or_create(pipeline_ids, create)?;
    attach(attempt, pipeline_id)?;
    report(head_sha, "pending", "GitLab verification running")?;
    announce(attempt, pipeline_id)
}

/// A branch that will not merge into the base cannot be verified against it,
/// and guessing at the conflict is not the bridge's to do. The author gets a
/// verdict they can act on instead of a run of whatever their branch was built
/// against months ago.
fn reject_unmergeable(
    pull_number: u64,
    head_sha: &str,
    base_sha: &str,
    entry: &LedgerEntry,
    report: impl FnOnce(&str, &str, &str) -> Result<()>,
    reject: impl FnOnce(u64, String) -> Result<()>,
) -> Result<()> {
    let detail = format!(
        "GitHub PR #{pull_number} does not merge into the verified base {base_sha}. Merge the \
         default branch into it and push; the verification runs the merge, not the head alone"
    );
    report(head_sha, "failure", &detail)?;
    reject(entry.attempt, detail)
}

fn reject_control_changes(
    pull_number: u64,
    head_sha: &str,
    entry: &LedgerEntry,
    paths: &[String],
    report: impl FnOnce(&str, &str, &str) -> Result<()>,
    reject: impl FnOnce(u64, String) -> Result<()>,
) -> Result<bool> {
    if paths.is_empty() {
        return Ok(false);
    }
    if entry.state == VerificationState::Rejected {
        return Ok(true);
    }

    let detail = format!(
        "GitHub PR #{pull_number} weakens the trusted CI judge in {}: an entry that already existed was changed or removed. Port these changes through a reviewed GitLab merge request",
        paths.join(", ")
    );
    report(head_sha, "failure", &detail)?;
    if entry.state == VerificationState::Verified {
        bail!(
            "verification {head_sha} attempt {} was already verified before its protected control-path change was rejected",
            entry.attempt
        );
    }
    reject(entry.attempt, detail)?;
    Ok(true)
}

fn observe_verification(
    head_sha: &str,
    pipeline_id: u64,
    mut observe: impl FnMut(u64) -> Result<PipelineObservation>,
    mut report: impl FnMut(&str, &str, &str) -> Result<()>,
    mut finish: impl FnMut(u64, VerificationState, Option<String>) -> Result<()>,
) -> Result<()> {
    match observe(pipeline_id)? {
        PipelineObservation::Running => Ok(()),
        PipelineObservation::Succeeded => {
            let detail = format!("GitLab pipeline {pipeline_id} passed");
            report(head_sha, "success", &detail)?;
            finish(pipeline_id, VerificationState::Verified, Some(detail))
        }
        PipelineObservation::Failed(status) => {
            let detail = format!("GitLab pipeline {pipeline_id} finished with {status}");
            let github_state = if status == "failed" {
                "failure"
            } else {
                "error"
            };
            report(head_sha, github_state, &detail)?;
            finish(pipeline_id, VerificationState::Rejected, Some(detail))
        }
        PipelineObservation::Invalid(detail) => {
            report(head_sha, "error", &detail)?;
            finish(pipeline_id, VerificationState::Rejected, Some(detail))
        }
    }
}

fn fast_forward_github_import(
    github_sha: &str,
    gitlab_base_sha: &str,
    branch: &str,
    merged_pull_request: impl FnOnce(&str) -> Result<Option<u64>>,
    refresh_heads: impl FnOnce() -> Result<(String, String)>,
    report_untrusted: impl FnOnce(&str) -> Result<()>,
    push_gitlab: impl FnOnce(&str, &str) -> Result<()>,
) -> Result<()> {
    let Some(pull_number) = merged_pull_request(github_sha)? else {
        let detail = format!(
            "GitHub head {github_sha} is not associated with a merged pull request targeting \
             {branch}"
        );
        report_untrusted(&detail)?;
        bail!("{detail}");
    };

    let (current_github, current_gitlab) = refresh_heads()?;
    if current_github != github_sha || current_gitlab != gitlab_base_sha {
        bail!(
            "repository heads changed before GitHub PR #{pull_number} import; fast-forward was \
             not attempted"
        );
    }

    push_gitlab(github_sha, branch)
}

fn require_sha(owner: &str, sha: &str) -> Result<()> {
    if !validate_sha(sha) {
        bail!("{owner} returned an invalid commit SHA: {sha:?}");
    }
    Ok(())
}

#[cfg(test)]
#[path = "reconcile_tests.rs"]
mod tests;

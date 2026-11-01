use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    path::Path,
};

use anyhow::{Context, Result, bail};
use kithara_devtools::lock::{FileLock, Wait};
use tracing::{info, warn};

use super::{
    api::{Github, Gitlab},
    checks,
    command::BridgeConfig,
    git::{GitRepo, Judged},
    ledger::{DefaultBranchStatus, Ledger, LedgerEntry},
    model::{
        Branches, Direction, PipelineObservation, PullRequest, Scope, VerificationState,
        direction_for, validate_sha,
    },
};
use crate::ci::run::PipelineKind;

pub(super) struct Bridge {
    config: BridgeConfig,
    github: Github,
    gitlab: Gitlab,
    repo: GitRepo,
}

/// One verification attempt: a pull-request head judged merged with a
/// default-branch commit, under the bridge configuration that names both
/// sides and links the pipeline.
struct VerificationAttempt<'a> {
    head_sha: &'a str,
    base_sha: &'a str,
    attempt: u64,
    scope: Scope,
    config: &'a BridgeConfig,
}

/// A verification an open pull request stands on: its head, the base it is
/// judged against and how much of the merge it checks. Every quarantine branch
/// that names none of these is left behind.
#[derive(Clone, Debug, Eq, PartialEq)]
struct LiveVerification {
    head_sha: String,
    base_sha: String,
    scope: Scope,
}

impl LiveVerification {
    /// Whether `reference` is a branch of this verification, in any naming
    /// scheme the bridge has used: both abbreviations appear in each.
    fn names(&self, reference: &str) -> bool {
        reference.contains(abbreviate(&self.head_sha))
            && reference.contains(abbreviate(&self.base_sha))
            && reference.contains(&format!("/{}-", attempt_name(Scope::Draft)))
                == (self.scope == Scope::Draft)
    }
}

struct ReconcileLock {
    _lock: FileLock,
}

impl ReconcileLock {
    fn acquire(state_dir: &Path) -> Result<Self> {
        let file = open_reconcile_lock(state_dir)?;
        let subject = format!("bridge state {}", state_dir.display());
        let holder = crate::job::lock_holder();
        let lock = FileLock::exclusive(
            file,
            &Wait {
                subject: &subject,
                holder: &holder,
            },
        )
        .with_context(|| format!("locking {subject}"))?;
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
            |base_sha| {
                let ledger = Ledger::new(&self.config.state_dir)?;
                report_default_branch(
                    base_sha,
                    &self.config,
                    &ledger,
                    |sha| self.gitlab.default_branch_pipeline(sha),
                    |id| self.gitlab.pipeline_observation(id, PipelineKind::Main),
                    |sha, state, detail, url| self.github.report_status(sha, state, detail, url),
                )
            },
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
                    .ensure_issue("GitHub and GitLab default branches diverged", &detail)?;
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
            Branches {
                github: &self.config.github_branch,
                gitlab: &self.config.gitlab_branch,
            },
            |sha| self.github.merged_pull_request(sha),
            || {
                self.repo.fetch(&self.github, &self.gitlab)?;
                Ok((self.github.head()?, self.gitlab.head()?))
            },
            |detail| {
                self.gitlab
                    .ensure_issue("Untrusted direct GitHub default-branch update", detail)
            },
            |sha, branch| self.repo.push_gitlab(&self.gitlab, sha, branch),
        )
    }

    fn verify_open_pulls(&self, base_sha: &str) -> Result<()> {
        let ledger = Ledger::new(&self.config.state_dir)?;
        let pulls = self.github.open_pull_requests()?;
        let mut live = Vec::with_capacity(pulls.len());
        for pull in &pulls {
            let verified_base =
                verification_base(pull.scope, ledger.latest_base(&pull.head_sha)?, base_sha);
            if let Err(error) = self.verify_pull(&ledger, pull, &verified_base, base_sha) {
                warn!(
                    pull_number = pull.number,
                    head_sha = %pull.head_sha,
                    base_sha = %verified_base,
                    %error,
                    "GitLab verification tick failed without changing its verdict"
                );
            }
            let entry = ledger.get(&pull.head_sha, &verified_base)?;
            let scope = entry.as_ref().map_or(pull.scope, |entry| entry.scope);
            if let Err(error) = checks::report_pull(
                &self.github,
                &self.gitlab,
                pull,
                entry.as_ref().and_then(|entry| entry.pipeline_id),
                scope,
            ) {
                warn!(
                    pull_number = pull.number,
                    head_sha = %pull.head_sha,
                    %error,
                    "pull-request job statuses could not be published"
                );
            }
            live.push(LiveVerification {
                head_sha: pull.head_sha.clone(),
                base_sha: verified_base,
                scope,
            });
        }
        if let Err(error) = sweep_quarantine_refs(
            &live,
            || self.repo.gitlab_quarantine_refs(&self.gitlab),
            |reference| self.gitlab.cancel_pipelines(reference),
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

    fn verify_pull(
        &self,
        ledger: &Ledger,
        pull: &PullRequest,
        base_sha: &str,
        target_sha: &str,
    ) -> Result<()> {
        require_sha("GitHub pull request", &pull.head_sha)?;
        let review_lines = self
            .repo
            .fetch_pull_head(&self.github, pull.number, &pull.head_sha)
            .and_then(|()| self.repo.review_size(target_sha, &pull.head_sha));
        self.github
            .report_check_status(&pull.head_sha, &checks::review_size(&review_lines))?;
        review_lines?;
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
        let entry = ledger.reserve(&pull.head_sha, base_sha, pull.scope)?;
        if reject_control_changes(
            pull.number,
            &pull.head_sha,
            &entry,
            &changed_controls,
            |sha, state, detail, url| self.github.report_status(sha, state, detail, url),
            |attempt, detail| ledger.reject(&pull.head_sha, base_sha, attempt, detail),
        )? {
            return Ok(());
        }
        if entry.state != VerificationState::Testing && entry.pipeline_id.is_none() {
            return Ok(());
        }

        let attempt = VerificationAttempt {
            head_sha: &pull.head_sha,
            base_sha,
            attempt: entry.attempt,
            scope: entry.scope,
            config: &self.config,
        };
        let Some(pipeline_id) = entry.pipeline_id else {
            let reference = quarantine_ref(&pull.head_sha, base_sha, entry.attempt, entry.scope);
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
                        |sha, state, detail, url| {
                            self.github.report_status(sha, state, detail, url)
                        },
                        |attempt, detail| ledger.reject(&pull.head_sha, base_sha, attempt, detail),
                    );
                }
            };
            self.repo.push_gitlab(&self.gitlab, &judged, &reference)?;
            let discovered =
                self.gitlab
                    .verification_pipelines(&reference, &pull.head_sha, base_sha)?;
            start_verification(
                &attempt,
                &discovered,
                ledger.last_pass(&pull.head_sha, base_sha)?.as_deref(),
                || {
                    self.gitlab
                        .create_pipeline(&reference, &pull.head_sha, base_sha, entry.scope)
                },
                |attempt, pipeline_id| {
                    ledger.attach(&pull.head_sha, base_sha, attempt, pipeline_id)
                },
                |sha, state, detail, url| self.github.report_status(sha, state, detail, url),
                |attempt, pipeline_id| {
                    ledger.announce(&pull.head_sha, base_sha, attempt, pipeline_id)
                },
            )?;
            return Ok(());
        };

        if !entry.announced {
            let last_pass = ledger.last_pass(&pull.head_sha, base_sha)?;
            self.github.report_status(
                &pull.head_sha,
                "pending",
                &scoped(
                    entry.scope,
                    pending_description(&self.config.github_branch, base_sha, last_pass.as_deref()),
                ),
                Some(&self.config.gitlab_pipeline_url(pipeline_id)),
            )?;
            ledger.announce(&pull.head_sha, base_sha, entry.attempt, pipeline_id)?;
            return Ok(());
        }

        observe_verification(
            &attempt,
            pipeline_id,
            entry.state,
            |id| self.gitlab.pipeline_observation(id, entry.scope.kind()),
            |sha, state, detail, url| self.github.report_status(sha, state, detail, url),
            |id, state, detail| {
                ledger.finish(&pull.head_sha, base_sha, entry.attempt, id, state, detail)
            },
            |id| ledger.release(&pull.head_sha, base_sha, entry.attempt, id),
        )
    }
}

/// Report and verify only while both default branches name the same commit:
/// otherwise the pipeline verdict could be attached to a different head.
fn reconcile_main_first(
    reconcile_main: impl FnOnce() -> Result<Option<String>>,
    report_default: impl FnOnce(&str) -> Result<()>,
    verify_pulls: impl FnOnce(&str) -> Result<()>,
) -> Result<()> {
    if let Some(base) = reconcile_main()? {
        if let Err(error) = report_default(&base) {
            warn!(%error, %base, "default-branch status tick failed");
        }
        if let Err(error) = verify_pulls(&base) {
            warn!(%error, %base, "pull-request verification tick failed");
        }
    }
    Ok(())
}

/// The ledger dedupes verdicts because statuses are append-only and capped per commit.
/// A commit stays watched after it stops being the head until its pipeline finishes,
/// so its pending status still resolves.
fn report_default_branch(
    head: &str,
    config: &BridgeConfig,
    ledger: &Ledger,
    pipeline: impl FnOnce(&str) -> Result<Option<u64>>,
    mut observe: impl FnMut(u64) -> Result<PipelineObservation>,
    mut report: impl FnMut(&str, &str, &str, Option<&str>) -> Result<()>,
) -> Result<()> {
    let posted = ledger.default_branch_statuses()?;
    let mut watched = posted
        .iter()
        .filter(|(_, status)| status.state == "pending")
        .map(|(sha, status)| (sha.as_str(), status.pipeline_id))
        .collect::<BTreeMap<_, _>>();
    if let Some(pipeline_id) = pipeline(head)? {
        watched.insert(head, pipeline_id);
    }
    for (sha, pipeline_id) in watched {
        if let Err(error) = settle_default_branch_commit(
            sha,
            pipeline_id,
            config,
            ledger,
            posted.get(sha),
            &mut observe,
            &mut report,
        ) {
            warn!(%error, %sha, pipeline_id, "default-branch commit reporting failed");
        }
    }
    for (sha, status) in posted {
        if status.state != "pending" && sha != head {
            ledger.forget_default_branch(&sha)?;
        }
    }
    Ok(())
}

fn settle_default_branch_commit(
    sha: &str,
    pipeline_id: u64,
    config: &BridgeConfig,
    ledger: &Ledger,
    posted: Option<&DefaultBranchStatus>,
    observe: &mut impl FnMut(u64) -> Result<PipelineObservation>,
    report: &mut impl FnMut(&str, &str, &str, Option<&str>) -> Result<()>,
) -> Result<()> {
    let (state, description) =
        default_branch_verdict(pipeline_id, observe(pipeline_id)?, &config.gitlab_branch);
    let status = DefaultBranchStatus {
        pipeline_id,
        state: state.into(),
        description,
    };
    if posted != Some(&status) {
        report(
            sha,
            &status.state,
            &status.description,
            Some(&config.gitlab_pipeline_url(pipeline_id)),
        )?;
        ledger.record_default_branch(sha, status)?;
    }
    Ok(())
}

fn default_branch_verdict(
    pipeline_id: u64,
    observation: PipelineObservation,
    gitlab_branch: &str,
) -> (&'static str, String) {
    match observation {
        PipelineObservation::Running => (
            "pending",
            format!("GitLab pipeline {pipeline_id} on {gitlab_branch} running"),
        ),
        PipelineObservation::Succeeded => (
            "success",
            format!("GitLab pipeline {pipeline_id} on {gitlab_branch} passed"),
        ),
        PipelineObservation::Failed(status) => (
            if status == "failed" {
                "failure"
            } else {
                "error"
            },
            format!("GitLab pipeline {pipeline_id} on {gitlab_branch} finished with {status}"),
        ),
        PipelineObservation::Cancelled => (
            "error",
            format!("GitLab pipeline {pipeline_id} on {gitlab_branch} was stopped"),
        ),
        PipelineObservation::Invalid(detail) => ("error", detail),
    }
}

fn pending_description(github_branch: &str, base_sha: &str, last_pass: Option<&str>) -> String {
    let base = abbreviate(base_sha);
    last_pass.map_or_else(
        || format!("GitLab verification of the merge with {github_branch} {base} running"),
        |old| {
            let old = abbreviate(old);
            format!(
                "Merging now is at risk: {github_branch} moved to {base} after the pass on {old}; \
                 re-checking the merge"
            )
        },
    )
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
fn quarantine_ref(head_sha: &str, base_sha: &str, attempt: u64, scope: Scope) -> String {
    format!(
        "quarantine/gh/{}-{}/{}-{attempt}",
        abbreviate(head_sha),
        abbreviate(base_sha),
        attempt_name(scope)
    )
}

/// The last segment of a verification branch, before its attempt number. A
/// draft's run is named apart, so a draft marked ready leaves it behind.
const fn attempt_name(scope: Scope) -> &'static str {
    match scope {
        Scope::Full => "attempt",
        Scope::Draft => "draft-attempt",
    }
}

/// The base a pull request is judged against. One ready for review follows
/// the default branch; a draft stays on the base it was last judged against
/// until its author pushes, so a moving default branch does not queue every
/// draft again.
fn verification_base(scope: Scope, latest: Option<String>, current: &str) -> String {
    match (scope, latest) {
        (Scope::Draft, Some(latest)) => latest,
        (Scope::Draft, None) | (Scope::Full, _) => current.to_owned(),
    }
}

/// A status description, marked when it speaks for a draft's narrower run.
fn scoped(scope: Scope, detail: String) -> String {
    match scope {
        Scope::Full => detail,
        Scope::Draft => format!("Draft, touched tests only: {detail}"),
    }
}

/// Twelve hex digits, the width git itself grows to on a repository this size.
/// Shorter reads better and collides sooner; a collision here would have to
/// land between two pull-request heads verified against the same base.
fn abbreviate(sha: &str) -> &str {
    &sha[..sha.len().min(12)]
}

/// The verification branches nothing will ever name again.
///
/// A quarantine ref is addressed by the pair it was judged for, so either side
/// moving leaves the old branch behind: the base moves when main advances, and
/// the head moves whenever the author pushes. Judging the base alone left the
/// second kind standing - nine of them held slots at once on a runner that
/// takes one job at a time, ahead of the runs people were waiting on. A draft
/// keeps the base it was judged against, and once it is ready its draft run is
/// left behind too.
fn superseded_quarantine_refs<'a>(refs: &'a [String], live: &[LiveVerification]) -> Vec<&'a str> {
    refs.iter()
        .map(String::as_str)
        .filter(|reference| {
            reference.starts_with("quarantine/")
                && !live
                    .iter()
                    .any(|verification| verification.names(reference))
        })
        .collect()
}

/// Cancel and drop the verification branches nothing will name again.
///
/// The bridge pushes one of these per attempt and never reads it back: the
/// verdict lives in the ledger, and `verify_pull` reserves against whatever
/// head and base are current now. Nothing addresses an orphan again and
/// nothing reads its pipeline, so what is left is exhaust - 197 of them had
/// piled up on `GitLab` by the time anyone counted. Deleting the branch does
/// not stop its run: a queued pipeline keeps its place in the resource group
/// after the ref it names is gone, so the cancel has to be its own call.
///
/// A failed cancel stops the pass before the delete. Removing the branch while
/// its pipeline still queues is the state this exists to prevent, and the ref
/// left in place is what the next tick finds it by.
fn sweep_quarantine_refs(
    live: &[LiveVerification],
    list: impl FnOnce() -> Result<Vec<String>>,
    cancel: impl Fn(&str) -> Result<()>,
    delete: impl FnOnce(&[&str]) -> Result<()>,
) -> Result<()> {
    let listed = list()?;
    let superseded = superseded_quarantine_refs(&listed, live);
    if superseded.is_empty() {
        return Ok(());
    }
    for reference in &superseded {
        cancel(reference)?;
    }
    delete(&superseded)?;
    info!(
        dropped = superseded.len(),
        "verification branches nothing will name again cancelled and removed"
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
    attempt: &VerificationAttempt<'_>,
    pipeline_ids: &[u64],
    last_pass: Option<&str>,
    create: impl FnOnce() -> Result<u64>,
    attach: impl FnOnce(u64, u64) -> Result<()>,
    report: impl FnOnce(&str, &str, &str, Option<&str>) -> Result<()>,
    announce: impl FnOnce(u64, u64) -> Result<()>,
) -> Result<()> {
    let pipeline_id = recover_or_create(pipeline_ids, create)?;
    attach(attempt.attempt, pipeline_id)?;
    report(
        attempt.head_sha,
        "pending",
        &scoped(
            attempt.scope,
            pending_description(&attempt.config.github_branch, attempt.base_sha, last_pass),
        ),
        Some(&attempt.config.gitlab_pipeline_url(pipeline_id)),
    )?;
    announce(attempt.attempt, pipeline_id)
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
    report: impl FnOnce(&str, &str, &str, Option<&str>) -> Result<()>,
    reject: impl FnOnce(u64, String) -> Result<()>,
) -> Result<()> {
    let detail = format!(
        "GitHub PR #{pull_number} does not merge into the verified base {base_sha}. Merge the \
         default branch into it and push; the verification runs the merge, not the head alone"
    );
    report(head_sha, "failure", &detail, None)?;
    reject(entry.attempt, detail)
}

fn reject_control_changes(
    pull_number: u64,
    head_sha: &str,
    entry: &LedgerEntry,
    paths: &[String],
    report: impl FnOnce(&str, &str, &str, Option<&str>) -> Result<()>,
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
    report(head_sha, "failure", &detail, None)?;
    if entry.state == VerificationState::Verified {
        bail!(
            "verification {head_sha} attempt {} was already verified before its protected control-path change was rejected",
            entry.attempt
        );
    }
    reject(entry.attempt, detail)?;
    Ok(true)
}

/// One tick of a verification that already owns a pipeline.
///
/// A stopped run is released rather than recorded. Six pull requests were
/// marked failed at once when the queue holding their runs was emptied: nothing
/// had been judged, yet every entry went terminal and no later tick addressed
/// them again. What a cancellation reports is that the branch is still
/// unverified, which is what the pull request is told while the next attempt
/// opens.
fn observe_verification(
    attempt: &VerificationAttempt<'_>,
    pipeline_id: u64,
    previous: VerificationState,
    observe: impl FnOnce(u64) -> Result<PipelineObservation>,
    mut report: impl FnMut(&str, &str, &str, Option<&str>) -> Result<()>,
    mut finish: impl FnMut(u64, VerificationState, Option<String>) -> Result<()>,
    release: impl FnOnce(u64) -> Result<()>,
) -> Result<()> {
    let url = attempt.config.gitlab_pipeline_url(pipeline_id);
    let branch = &attempt.config.github_branch;
    let base = abbreviate(attempt.base_sha);
    let observation = observe(pipeline_id)?;
    let state = match &observation {
        PipelineObservation::Running => Some(VerificationState::Testing),
        PipelineObservation::Succeeded => Some(VerificationState::Verified),
        PipelineObservation::Failed(_) | PipelineObservation::Invalid(_) => {
            Some(VerificationState::Rejected)
        }
        PipelineObservation::Cancelled => None,
    };
    if state == Some(previous) {
        return Ok(());
    }
    match observation {
        PipelineObservation::Running => {
            let detail = scoped(
                attempt.scope,
                format!("GitLab pipeline {pipeline_id} retry running merged with {branch} {base}"),
            );
            report(attempt.head_sha, "pending", &detail, Some(&url))?;
            finish(pipeline_id, VerificationState::Testing, Some(detail))
        }
        PipelineObservation::Succeeded => {
            let detail = scoped(
                attempt.scope,
                format!("GitLab pipeline {pipeline_id} passed merged with {branch} {base}"),
            );
            report(attempt.head_sha, "success", &detail, Some(&url))?;
            finish(pipeline_id, VerificationState::Verified, Some(detail))
        }
        PipelineObservation::Cancelled => {
            let detail = scoped(
                attempt.scope,
                format!(
                    "GitLab pipeline {pipeline_id} was stopped; a new run against {branch} {base} \
                     will be started"
                ),
            );
            report(attempt.head_sha, "pending", &detail, Some(&url))?;
            release(pipeline_id)
        }
        PipelineObservation::Failed(status) => {
            let detail = scoped(
                attempt.scope,
                format!(
                    "GitLab pipeline {pipeline_id} finished with {status} merged with {branch} \
                     {base}"
                ),
            );
            let github_state = if status == "failed" {
                "failure"
            } else {
                "error"
            };
            report(attempt.head_sha, github_state, &detail, Some(&url))?;
            finish(pipeline_id, VerificationState::Rejected, Some(detail))
        }
        PipelineObservation::Invalid(detail) => {
            report(attempt.head_sha, "error", &detail, Some(&url))?;
            finish(pipeline_id, VerificationState::Rejected, Some(detail))
        }
    }
}

fn fast_forward_github_import(
    github_sha: &str,
    gitlab_base_sha: &str,
    branches: Branches<'_>,
    merged_pull_request: impl FnOnce(&str) -> Result<Option<u64>>,
    refresh_heads: impl FnOnce() -> Result<(String, String)>,
    report_untrusted: impl FnOnce(&str) -> Result<()>,
    push_gitlab: impl FnOnce(&str, &str) -> Result<()>,
) -> Result<()> {
    let Some(pull_number) = merged_pull_request(github_sha)? else {
        let github_branch = branches.github;
        let detail = format!(
            "GitHub head {github_sha} is not associated with a merged pull request targeting \
             {github_branch}"
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

    push_gitlab(github_sha, branches.gitlab)
}

fn require_sha(owner: &str, sha: &str) -> Result<()> {
    if !validate_sha(sha) {
        bail!("{owner} returned an invalid commit SHA: {sha:?}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        cell::{Cell, RefCell},
        collections::BTreeMap,
        rc::Rc,
    };

    use super::{super::ledger::DefaultBranchStatus, *};
    use crate::consts;

    fn status_config() -> BridgeConfig {
        let mut config: BridgeConfig = toml::from_str(include_str!(
            "../../../../.config/bridge/config.example.toml"
        ))
        .unwrap();
        config.github_branch = "release".into();
        config.gitlab_branch = "stable".into();
        config.gitlab_url = "https://gitlab.example/".parse().unwrap();
        config.gitlab_project_path = "team/audio".into();
        config
    }

    #[test]
    fn pending_without_an_older_pass_names_the_branch_and_abbreviated_base() {
        assert_eq!(
            pending_description("release", consts::STATUS_BASE, None),
            "GitLab verification of the merge with release 0123456789ab running"
        );
    }

    #[test]
    fn pending_after_an_older_pass_explains_the_moved_base_and_merge_risk() {
        assert_eq!(
            pending_description(
                "release",
                consts::STATUS_BASE,
                Some(consts::STATUS_OLD_BASE)
            ),
            "Merging now is at risk: release moved to 0123456789ab after the pass on 89abcdef0123; re-checking the merge"
        );
    }

    #[test]
    fn starting_or_recovering_a_pipeline_reports_the_current_merge_base_and_url() {
        for discovered in [Vec::new(), vec![42]] {
            let reports = RefCell::new(Vec::new());
            start_verification(
                &VerificationAttempt {
                    head_sha: "head",
                    base_sha: consts::STATUS_BASE,
                    attempt: 1,
                    scope: Scope::Full,
                    config: &status_config(),
                },
                &discovered,
                None,
                || {
                    assert!(discovered.is_empty());
                    Ok(42)
                },
                |attempt, id| {
                    assert_eq!((attempt, id), (1, 42));
                    Ok(())
                },
                |sha, state, description, url| {
                    reports.borrow_mut().push((
                        sha.to_owned(),
                        state.to_owned(),
                        description.to_owned(),
                        url.map(str::to_owned),
                    ));
                    Ok(())
                },
                |attempt, id| {
                    assert_eq!((attempt, id), (1, 42));
                    Ok(())
                },
            )
            .unwrap();

            assert_eq!(
                *reports.borrow(),
                [(
                    "head".into(),
                    "pending".into(),
                    "GitLab verification of the merge with release 0123456789ab running".into(),
                    Some(consts::STATUS_URL.into()),
                )]
            );
        }
    }

    #[test]
    fn starting_or_recovering_a_pipeline_reports_the_older_pass_and_merge_risk() {
        for discovered in [Vec::new(), vec![42]] {
            let reports = RefCell::new(Vec::new());
            start_verification(
                &VerificationAttempt {
                    head_sha: "head",
                    base_sha: consts::STATUS_BASE,
                    attempt: 1,
                    scope: Scope::Full,
                    config: &status_config(),
                },
                &discovered,
                Some(consts::STATUS_OLD_BASE),
                || {
                    assert!(discovered.is_empty());
                    Ok(42)
                },
                |_, _| Ok(()),
                |sha, state, description, url| {
                    reports.borrow_mut().push((
                        sha.to_owned(),
                        state.to_owned(),
                        description.to_owned(),
                        url.map(str::to_owned),
                    ));
                    Ok(())
                },
                |_, _| Ok(()),
            )
            .unwrap();

            assert_eq!(
                *reports.borrow(),
                [(
                    "head".into(),
                    "pending".into(),
                    "Merging now is at risk: release moved to 0123456789ab after the pass on 89abcdef0123; re-checking the merge".into(),
                    Some(consts::STATUS_URL.into()),
                )]
            );
        }
    }

    fn observed_pr_reports(
        observation: PipelineObservation,
    ) -> Vec<(String, String, String, Option<String>)> {
        let mut reports = Vec::new();
        observe_verification(
            &VerificationAttempt {
                head_sha: "head",
                base_sha: consts::STATUS_BASE,
                attempt: 1,
                scope: Scope::Full,
                config: &status_config(),
            },
            42,
            VerificationState::Testing,
            |_| Ok(observation),
            |sha, state, description, url| {
                reports.push((
                    sha.to_owned(),
                    state.to_owned(),
                    description.to_owned(),
                    url.map(str::to_owned),
                ));
                Ok(())
            },
            |_, _, _| Ok(()),
            |_| Ok(()),
        )
        .unwrap();
        reports
    }

    #[test]
    fn a_pr_pass_names_the_merge_base_and_links_the_pipeline() {
        assert_eq!(
            observed_pr_reports(PipelineObservation::Succeeded),
            [(
                "head".into(),
                "success".into(),
                "GitLab pipeline 42 passed merged with release 0123456789ab".into(),
                Some(consts::STATUS_URL.into()),
            )]
        );
    }

    #[test]
    fn a_pr_failure_names_the_merge_base_and_links_the_pipeline() {
        assert_eq!(
            observed_pr_reports(PipelineObservation::Failed("failed".into())),
            [(
                "head".into(),
                "failure".into(),
                "GitLab pipeline 42 finished with failed merged with release 0123456789ab".into(),
                Some(consts::STATUS_URL.into()),
            )]
        );
    }

    #[test]
    fn other_terminal_pr_statuses_remain_errors_and_name_the_merge_base() {
        for status in ["skipped", "manual"] {
            assert_eq!(
                observed_pr_reports(PipelineObservation::Failed(status.into())),
                [(
                    "head".into(),
                    "error".into(),
                    format!(
                        "GitLab pipeline 42 finished with {status} merged with release 0123456789ab"
                    ),
                    Some(consts::STATUS_URL.into()),
                )]
            );
        }
    }

    #[test]
    fn a_stopped_pr_pipeline_names_the_next_merge_base_and_stays_pending() {
        assert_eq!(
            observed_pr_reports(PipelineObservation::Cancelled),
            [(
                "head".into(),
                "pending".into(),
                "GitLab pipeline 42 was stopped; a new run against release 0123456789ab will be started".into(),
                Some(consts::STATUS_URL.into()),
            )]
        );
    }

    #[test]
    fn invalid_pr_proof_preserves_the_detail_and_links_the_pipeline() {
        assert_eq!(
            observed_pr_reports(PipelineObservation::Invalid("missing child".into())),
            [(
                "head".into(),
                "error".into(),
                "missing child".into(),
                Some(consts::STATUS_URL.into()),
            )]
        );
    }

    #[derive(Debug, Eq, PartialEq)]
    enum DefaultBranchAction {
        Pipeline(String),
        Observe(u64),
        Report(String, String, String, Option<String>),
    }

    fn default_branch_actions(
        head: &str,
        config: &BridgeConfig,
        ledger: &Ledger,
        pipeline: Option<u64>,
        mut observe: impl FnMut(u64) -> Result<PipelineObservation>,
    ) -> Result<Vec<DefaultBranchAction>> {
        let actions = RefCell::new(Vec::new());
        report_default_branch(
            head,
            config,
            ledger,
            |sha| {
                actions
                    .borrow_mut()
                    .push(DefaultBranchAction::Pipeline(sha.into()));
                Ok(pipeline)
            },
            |id| {
                actions.borrow_mut().push(DefaultBranchAction::Observe(id));
                observe(id)
            },
            |sha, state, description, url| {
                actions.borrow_mut().push(DefaultBranchAction::Report(
                    sha.into(),
                    state.into(),
                    description.into(),
                    url.map(str::to_owned),
                ));
                Ok(())
            },
        )?;
        Ok(actions.into_inner())
    }

    #[test]
    fn default_branch_verdicts_map_each_observation_with_its_url() {
        let config = status_config();
        let pipeline_id = 42;
        let gitlab_branch = &config.gitlab_branch;
        for (observation, state, description) in [
            (
                PipelineObservation::Failed("failed".into()),
                "failure",
                format!("GitLab pipeline {pipeline_id} on {gitlab_branch} finished with failed"),
            ),
            (
                PipelineObservation::Failed("skipped".into()),
                "error",
                format!("GitLab pipeline {pipeline_id} on {gitlab_branch} finished with skipped"),
            ),
            (
                PipelineObservation::Cancelled,
                "error",
                format!("GitLab pipeline {pipeline_id} on {gitlab_branch} was stopped"),
            ),
            (
                PipelineObservation::Invalid("missing child".into()),
                "error",
                "missing child".into(),
            ),
        ] {
            let directory = tempfile::tempdir().unwrap();
            let ledger = Ledger::new(directory.path()).unwrap();
            assert_eq!(
                default_branch_actions("a", &config, &ledger, Some(pipeline_id), |_| {
                    Ok(observation.clone())
                })
                .unwrap(),
                [
                    DefaultBranchAction::Pipeline("a".into()),
                    DefaultBranchAction::Observe(pipeline_id),
                    DefaultBranchAction::Report(
                        "a".into(),
                        state.into(),
                        description,
                        Some(config.gitlab_pipeline_url(pipeline_id)),
                    ),
                ]
            );
        }
    }

    #[test]
    fn a_running_head_is_posted_once() {
        let directory = tempfile::tempdir().unwrap();
        let ledger = Ledger::new(directory.path()).unwrap();
        let config = status_config();
        let mut actions = Vec::new();
        for _ in 0..2 {
            actions.extend(
                default_branch_actions("a", &config, &ledger, Some(1), |_| {
                    Ok(PipelineObservation::Running)
                })
                .unwrap(),
            );
        }

        assert_eq!(
            actions,
            [
                DefaultBranchAction::Pipeline("a".into()),
                DefaultBranchAction::Observe(1),
                DefaultBranchAction::Report(
                    "a".into(),
                    "pending".into(),
                    "GitLab pipeline 1 on stable running".into(),
                    Some(config.gitlab_pipeline_url(1)),
                ),
                DefaultBranchAction::Pipeline("a".into()),
                DefaultBranchAction::Observe(1),
            ]
        );
    }

    #[test]
    fn a_head_verdict_follows_its_pending() {
        let directory = tempfile::tempdir().unwrap();
        let ledger = Ledger::new(directory.path()).unwrap();
        let config = status_config();
        let mut actions = default_branch_actions("a", &config, &ledger, Some(1), |_| {
            Ok(PipelineObservation::Running)
        })
        .unwrap();
        actions.extend(
            default_branch_actions("a", &config, &ledger, Some(1), |_| {
                Ok(PipelineObservation::Succeeded)
            })
            .unwrap(),
        );

        assert_eq!(
            actions,
            [
                DefaultBranchAction::Pipeline("a".into()),
                DefaultBranchAction::Observe(1),
                DefaultBranchAction::Report(
                    "a".into(),
                    "pending".into(),
                    "GitLab pipeline 1 on stable running".into(),
                    Some(config.gitlab_pipeline_url(1)),
                ),
                DefaultBranchAction::Pipeline("a".into()),
                DefaultBranchAction::Observe(1),
                DefaultBranchAction::Report(
                    "a".into(),
                    "success".into(),
                    "GitLab pipeline 1 on stable passed".into(),
                    Some(config.gitlab_pipeline_url(1)),
                ),
            ]
        );
        assert_eq!(
            ledger.default_branch_statuses().unwrap(),
            BTreeMap::from([(
                "a".into(),
                DefaultBranchStatus {
                    pipeline_id: 1,
                    state: "success".into(),
                    description: "GitLab pipeline 1 on stable passed".into(),
                },
            )])
        );
    }

    #[test]
    fn a_commit_that_stopped_being_the_head_still_gets_its_verdict() {
        let directory = tempfile::tempdir().unwrap();
        let ledger = Ledger::new(directory.path()).unwrap();
        let config = status_config();
        default_branch_actions("a", &config, &ledger, Some(1), |_| {
            Ok(PipelineObservation::Running)
        })
        .unwrap();
        let actions = default_branch_actions("b", &config, &ledger, Some(2), |id| {
            Ok(if id == 1 {
                PipelineObservation::Failed("failed".into())
            } else {
                PipelineObservation::Running
            })
        })
        .unwrap();

        assert_eq!(
            actions,
            [
                DefaultBranchAction::Pipeline("b".into()),
                DefaultBranchAction::Observe(1),
                DefaultBranchAction::Report(
                    "a".into(),
                    "failure".into(),
                    "GitLab pipeline 1 on stable finished with failed".into(),
                    Some(config.gitlab_pipeline_url(1)),
                ),
                DefaultBranchAction::Observe(2),
                DefaultBranchAction::Report(
                    "b".into(),
                    "pending".into(),
                    "GitLab pipeline 2 on stable running".into(),
                    Some(config.gitlab_pipeline_url(2)),
                ),
            ]
        );
    }

    #[test]
    fn a_settled_commit_off_the_head_is_forgotten_unobserved() {
        let directory = tempfile::tempdir().unwrap();
        let ledger = Ledger::new(directory.path()).unwrap();
        let config = status_config();
        default_branch_actions("a", &config, &ledger, Some(1), |_| {
            Ok(PipelineObservation::Running)
        })
        .unwrap();
        default_branch_actions("b", &config, &ledger, Some(2), |id| {
            Ok(if id == 1 {
                PipelineObservation::Failed("failed".into())
            } else {
                PipelineObservation::Running
            })
        })
        .unwrap();
        let actions = default_branch_actions("b", &config, &ledger, Some(2), |_| {
            Ok(PipelineObservation::Running)
        })
        .unwrap();

        assert_eq!(
            actions,
            [
                DefaultBranchAction::Pipeline("b".into()),
                DefaultBranchAction::Observe(2),
            ]
        );
        assert_eq!(
            ledger.default_branch_statuses().unwrap(),
            BTreeMap::from([(
                "b".into(),
                DefaultBranchStatus {
                    pipeline_id: 2,
                    state: "pending".into(),
                    description: "GitLab pipeline 2 on stable running".into(),
                },
            )])
        );
    }

    #[test]
    fn a_new_pipeline_on_the_same_head_is_posted_again() {
        let directory = tempfile::tempdir().unwrap();
        let ledger = Ledger::new(directory.path()).unwrap();
        let config = status_config();
        let mut actions = Vec::new();
        for pipeline_id in [1, 2] {
            actions.extend(
                default_branch_actions("a", &config, &ledger, Some(pipeline_id), |_| {
                    Ok(PipelineObservation::Running)
                })
                .unwrap(),
            );
        }

        assert_eq!(
            actions,
            [
                DefaultBranchAction::Pipeline("a".into()),
                DefaultBranchAction::Observe(1),
                DefaultBranchAction::Report(
                    "a".into(),
                    "pending".into(),
                    "GitLab pipeline 1 on stable running".into(),
                    Some(config.gitlab_pipeline_url(1)),
                ),
                DefaultBranchAction::Pipeline("a".into()),
                DefaultBranchAction::Observe(2),
                DefaultBranchAction::Report(
                    "a".into(),
                    "pending".into(),
                    "GitLab pipeline 2 on stable running".into(),
                    Some(config.gitlab_pipeline_url(2)),
                ),
            ]
        );
        assert_eq!(
            ledger.default_branch_statuses().unwrap().get("a"),
            Some(&DefaultBranchStatus {
                pipeline_id: 2,
                state: "pending".into(),
                description: "GitLab pipeline 2 on stable running".into(),
            })
        );
    }

    #[test]
    fn one_broken_commit_does_not_hide_the_head() {
        let directory = tempfile::tempdir().unwrap();
        let ledger = Ledger::new(directory.path()).unwrap();
        let config = status_config();
        let pending = DefaultBranchStatus {
            pipeline_id: 1,
            state: "pending".into(),
            description: "GitLab pipeline 1 on stable running".into(),
        };
        ledger.record_default_branch("a", pending.clone()).unwrap();
        let result = default_branch_actions("b", &config, &ledger, Some(2), |id| {
            if id == 1 {
                bail!("old pipeline observation failed");
            }
            Ok(PipelineObservation::Succeeded)
        });

        assert!(result.is_ok());
        assert_eq!(
            result.unwrap(),
            [
                DefaultBranchAction::Pipeline("b".into()),
                DefaultBranchAction::Observe(1),
                DefaultBranchAction::Observe(2),
                DefaultBranchAction::Report(
                    "b".into(),
                    "success".into(),
                    "GitLab pipeline 2 on stable passed".into(),
                    Some(config.gitlab_pipeline_url(2)),
                ),
            ]
        );
        assert_eq!(
            ledger.default_branch_statuses().unwrap().get("a"),
            Some(&pending)
        );
    }

    #[test]
    fn no_pipeline_and_nothing_posted_does_nothing() {
        let directory = tempfile::tempdir().unwrap();
        let ledger = Ledger::new(directory.path()).unwrap();
        assert_eq!(
            default_branch_actions("a", &status_config(), &ledger, None, |_| {
                Ok(PipelineObservation::Running)
            })
            .unwrap(),
            [DefaultBranchAction::Pipeline("a".into())]
        );
    }

    #[test]
    fn a_default_branch_reporting_error_still_runs_pull_verification_on_the_same_tick() {
        let actions = RefCell::new(Vec::new());
        let result = reconcile_main_first(
            || Ok(Some("base".into())),
            |base| {
                assert_eq!(base, "base");
                actions.borrow_mut().push("default");
                bail!("temporary default-branch API error")
            },
            |base| {
                assert_eq!(base, "base");
                actions.borrow_mut().push("pulls");
                Ok(())
            },
        );
        assert!(result.is_ok());
        assert_eq!(*actions.borrow(), ["default", "pulls"]);
    }

    #[derive(Debug, Eq, PartialEq)]
    enum ImportAction {
        CheckProvenance,
        RefreshHeads,
        Push(String),
    }

    #[derive(Debug, Eq, PartialEq)]
    enum VerificationAction {
        Create,
        Attach(u64, u64),
        Announce(u64, u64),
        Observe(u64),
        Report(String, String),
        Finish(VerificationState),
        Release(u64),
        Reject(u64),
    }

    #[test]
    fn main_reconciliation_precedes_verification_and_fast_forward_skips_it() {
        let actions = RefCell::new(Vec::new());
        reconcile_main_first(
            || {
                actions.borrow_mut().push("main");
                Ok(None)
            },
            |_| panic!("a fast-forward tick must not report the default branch"),
            |_| {
                actions.borrow_mut().push("verification");
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(*actions.borrow(), ["main"]);
    }

    #[test]
    fn equal_main_runs_verification_after_reconciliation() {
        let actions = RefCell::new(Vec::new());
        reconcile_main_first(
            || {
                actions.borrow_mut().push("main");
                Ok(Some("base".into()))
            },
            |_| Ok(()),
            |base| {
                assert_eq!(base, "base");
                actions.borrow_mut().push("verification");
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(*actions.borrow(), ["main", "verification"]);
    }

    #[test]
    fn verifier_errors_do_not_fail_completed_main_reconciliation() {
        reconcile_main_first(
            || Ok(Some("base".into())),
            |_| Ok(()),
            |_| bail!("transient GitHub error"),
        )
        .unwrap();
    }

    #[test]
    fn one_state_directory_serializes_all_reconciliation_keys() {
        let directory = tempfile::tempdir().unwrap();
        let first = ReconcileLock::acquire(directory.path()).unwrap();

        assert!(
            ReconcileLock::try_acquire(directory.path())
                .unwrap()
                .is_none()
        );
        drop(first);
        assert!(
            ReconcileLock::try_acquire(directory.path())
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn a_new_verification_attaches_then_posts_to_exact_head_without_observing() {
        let head = "0123456789abcdef0123456789abcdef01234567";
        let actions = RefCell::new(Vec::new());
        start_verification(
            &VerificationAttempt {
                head_sha: head,
                base_sha: "base",
                attempt: 3,
                scope: Scope::Full,
                config: &status_config(),
            },
            &[],
            None,
            || {
                actions.borrow_mut().push(VerificationAction::Create);
                Ok(42)
            },
            |attempt, id| {
                actions
                    .borrow_mut()
                    .push(VerificationAction::Attach(attempt, id));
                Ok(())
            },
            |sha, state, _, _| {
                assert_eq!(sha, head);
                actions
                    .borrow_mut()
                    .push(VerificationAction::Report(sha.into(), state.into()));
                Ok(())
            },
            |attempt, id| {
                actions
                    .borrow_mut()
                    .push(VerificationAction::Announce(attempt, id));
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(
            *actions.borrow(),
            [
                VerificationAction::Create,
                VerificationAction::Attach(3, 42),
                VerificationAction::Report(head.into(), "pending".into()),
                VerificationAction::Announce(3, 42),
            ]
        );
    }

    #[test]
    fn one_later_tick_observes_once_and_running_keeps_testing() {
        let calls = RefCell::new(0);
        observe_verification(
            &VerificationAttempt {
                head_sha: "head",
                base_sha: "base",
                attempt: 1,
                scope: Scope::Full,
                config: &status_config(),
            },
            42,
            VerificationState::Testing,
            |id| {
                assert_eq!(id, 42);
                *calls.borrow_mut() += 1;
                Ok(PipelineObservation::Running)
            },
            |_, _, _, _| panic!("running is not a new commit-status transition"),
            |_, _, _| panic!("running must stay Testing"),
            |_| panic!("a running pipeline was not stopped"),
        )
        .unwrap();
        assert_eq!(*calls.borrow(), 1);
    }

    #[test]
    fn job_retry_statuses_follow_the_same_pipeline_and_do_not_repeat_reports() {
        let directory = tempfile::tempdir().unwrap();
        let ledger = Ledger::new(directory.path()).unwrap();
        let config = status_config();
        ledger.reserve("head", "base", Scope::Full).unwrap();
        ledger.attach("head", "base", 1, 42).unwrap();
        ledger.announce("head", "base", 1, 42).unwrap();
        let mut reports = Vec::new();
        for observation in [
            PipelineObservation::Failed("failed".into()),
            PipelineObservation::Failed("failed".into()),
            PipelineObservation::Running,
            PipelineObservation::Running,
            PipelineObservation::Succeeded,
            PipelineObservation::Succeeded,
            PipelineObservation::Running,
            PipelineObservation::Failed("failed".into()),
            PipelineObservation::Succeeded,
        ] {
            let entry = ledger.get("head", "base").unwrap().unwrap();
            observe_verification(
                &VerificationAttempt {
                    head_sha: "head",
                    base_sha: "base",
                    attempt: entry.attempt,
                    scope: entry.scope,
                    config: &config,
                },
                42,
                entry.state,
                |_| Ok(observation),
                |sha, state, _, url| {
                    assert_eq!(sha, "head");
                    assert_eq!(url, Some(config.gitlab_pipeline_url(42).as_str()));
                    reports.push(state.to_owned());
                    Ok(())
                },
                |id, state, detail| ledger.finish("head", "base", 1, id, state, detail),
                |_| panic!("a job retry must keep its pipeline"),
            )
            .unwrap();
        }
        assert_eq!(
            reports,
            [
                "failure", "pending", "success", "pending", "failure", "success"
            ]
        );
        let entry = ledger.get("head", "base").unwrap().unwrap();
        assert_eq!(entry.state, VerificationState::Verified);
        assert_eq!(entry.pipeline_id, Some(42));
        assert_eq!(entry.attempt, 1);
    }

    #[test]
    fn success_is_posted_before_verified() {
        let actions = RefCell::new(Vec::new());
        observe_verification(
            &VerificationAttempt {
                head_sha: "head",
                base_sha: "base",
                attempt: 1,
                scope: Scope::Full,
                config: &status_config(),
            },
            42,
            VerificationState::Testing,
            |id| {
                actions.borrow_mut().push(VerificationAction::Observe(id));
                Ok(PipelineObservation::Succeeded)
            },
            |sha, state, _, _| {
                actions
                    .borrow_mut()
                    .push(VerificationAction::Report(sha.into(), state.into()));
                Ok(())
            },
            |_, state, _| {
                actions.borrow_mut().push(VerificationAction::Finish(state));
                Ok(())
            },
            |_| panic!("a passing pipeline was not stopped"),
        )
        .unwrap();

        assert_eq!(
            *actions.borrow(),
            [
                VerificationAction::Observe(42),
                VerificationAction::Report("head".into(), "success".into()),
                VerificationAction::Finish(VerificationState::Verified),
            ]
        );
    }

    #[test]
    fn failed_and_invalid_proof_post_a_verdict_before_rejection() {
        for (observation, github_state) in [
            (PipelineObservation::Failed("failed".into()), "failure"),
            (
                PipelineObservation::Invalid("missing child".into()),
                "error",
            ),
        ] {
            let actions = RefCell::new(Vec::new());
            observe_verification(
                &VerificationAttempt {
                    head_sha: "head",
                    base_sha: "base",
                    attempt: 1,
                    scope: Scope::Full,
                    config: &status_config(),
                },
                42,
                VerificationState::Testing,
                |_| Ok(observation.clone()),
                |_, state, _, _| {
                    actions
                        .borrow_mut()
                        .push(VerificationAction::Report("head".into(), state.into()));
                    Ok(())
                },
                |_, state, _| {
                    actions.borrow_mut().push(VerificationAction::Finish(state));
                    Ok(())
                },
                |_| panic!("a verdict is not a stopped run"),
            )
            .unwrap();
            assert_eq!(
                *actions.borrow(),
                [
                    VerificationAction::Report("head".into(), github_state.into()),
                    VerificationAction::Finish(VerificationState::Rejected),
                ]
            );
        }
    }

    /// Six pull requests were marked failed at once when the queue they sat in was
    /// emptied. Nothing had run, so nothing had been judged, yet the ledger held
    /// every one of them terminal and no tick addressed them again. A cancellation
    /// is the absence of a verdict: the verification stays open and says so.
    #[test]
    fn a_cancelled_pipeline_reopens_the_verification_instead_of_rejecting_it() {
        let actions = RefCell::new(Vec::new());
        observe_verification(
            &VerificationAttempt {
                head_sha: "head",
                base_sha: "base",
                attempt: 1,
                scope: Scope::Full,
                config: &status_config(),
            },
            42,
            VerificationState::Testing,
            |id| {
                actions.borrow_mut().push(VerificationAction::Observe(id));
                Ok(PipelineObservation::Cancelled)
            },
            |sha, state, _, _| {
                actions
                    .borrow_mut()
                    .push(VerificationAction::Report(sha.into(), state.into()));
                Ok(())
            },
            |_, _, _| panic!("a cancellation is not a verdict"),
            |id| {
                actions.borrow_mut().push(VerificationAction::Release(id));
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(
            *actions.borrow(),
            [
                VerificationAction::Observe(42),
                VerificationAction::Report("head".into(), "pending".into()),
                VerificationAction::Release(42),
            ]
        );
    }

    #[test]
    fn transient_observation_errors_do_not_manufacture_rejection() {
        let error = observe_verification(
            &VerificationAttempt {
                head_sha: "head",
                base_sha: "base",
                attempt: 1,
                scope: Scope::Full,
                config: &status_config(),
            },
            42,
            VerificationState::Testing,
            |_| bail!("temporary GitLab API failure"),
            |_, _, _, _| panic!("an API error is not a verdict"),
            |_, _, _| panic!("an API error must leave Testing unchanged"),
            |_| panic!("an API error did not stop the pipeline"),
        )
        .unwrap_err();
        assert!(error.to_string().contains("temporary GitLab API failure"));
    }

    #[test]
    fn a_lost_create_response_is_recovered_without_a_second_pipeline() {
        let pipelines = RefCell::new(Vec::new());
        let creates = Cell::new(0);
        let first_discovery = pipelines.borrow().clone();
        let first = recover_or_create(&first_discovery, || {
            creates.set(creates.get() + 1);
            pipelines.borrow_mut().push(42);
            bail!("pipeline was created but its response was lost")
        });
        assert!(first.is_err());

        let second_discovery = pipelines.borrow().clone();
        let recovered = recover_or_create(&second_discovery, || {
            panic!("recovery must not create a second pipeline")
        })
        .unwrap();

        assert_eq!(recovered, 42);
        assert_eq!(creates.get(), 1);
    }

    #[test]
    fn ambiguous_recovery_fails_closed_without_creating() {
        let error =
            recover_or_create(&[41, 42], || panic!("ambiguity must not create")).unwrap_err();
        assert!(error.to_string().contains("multiple pipelines"));
    }

    #[test]
    fn an_attempt_ref_changes_only_when_retry_advances_the_generation() {
        assert_eq!(
            quarantine_ref("head", "base", 1, Scope::Full),
            "quarantine/gh/head-base/attempt-1"
        );
        assert_eq!(
            quarantine_ref("head", "base", 2, Scope::Full),
            "quarantine/gh/head-base/attempt-2"
        );
    }

    /// The name is read in GitLab's interface, where two full shas came to 101
    /// characters and broke the layout of every list the branch appeared in.
    #[test]
    fn an_attempt_ref_abbreviates_both_shas() {
        let reference = quarantine_ref(
            "1dba4b9b0689ca0e12a88093f7321fb4c432636e",
            "fe3c9e2d92564790b24d9d9d27f6f08d2f39af29",
            1,
            Scope::Full,
        );

        assert_eq!(
            reference,
            "quarantine/gh/1dba4b9b0689-fe3c9e2d9256/attempt-1"
        );
    }

    /// Shortening must not merge two runs into one branch. Both sides of the pair
    /// have to keep separating them — a pull request rebased onto a newer base is
    /// a different verification, not the same one again.
    #[test]
    fn each_side_of_the_pair_separates_one_run_from_another() {
        let head = "1dba4b9b0689ca0e12a88093f7321fb4c432636e";
        let base = "fe3c9e2d92564790b24d9d9d27f6f08d2f39af29";
        let other_head = "1dba4b9b0000ca0e12a88093f7321fb4c432636e";
        let other_base = "fe3c9e2d00004790b24d9d9d27f6f08d2f39af29";

        assert_ne!(
            quarantine_ref(head, base, 1, Scope::Full),
            quarantine_ref(other_head, base, 1, Scope::Full)
        );
        assert_ne!(
            quarantine_ref(head, base, 1, Scope::Full),
            quarantine_ref(head, other_base, 1, Scope::Full)
        );
    }

    #[test]
    fn control_path_changes_post_failure_and_reject_without_a_pipeline() {
        let directory = tempfile::tempdir().unwrap();
        let ledger = Ledger::new(directory.path()).unwrap();
        let entry = ledger.reserve("head", "base", Scope::Full).unwrap();
        let actions = RefCell::new(Vec::new());

        let handled = reject_control_changes(
            427,
            "head",
            &entry,
            &[".gitlab-ci.yml".into()],
            |sha, state, _, _| {
                actions
                    .borrow_mut()
                    .push(VerificationAction::Report(sha.into(), state.into()));
                Ok(())
            },
            |attempt, detail| {
                actions
                    .borrow_mut()
                    .push(VerificationAction::Reject(attempt));
                ledger.reject("head", "base", attempt, detail)
            },
        )
        .unwrap();

        assert!(handled);
        assert_eq!(
            *actions.borrow(),
            [
                VerificationAction::Report("head".into(), "failure".into()),
                VerificationAction::Reject(1),
            ]
        );
        let rejected = ledger.get("head", "base").unwrap().unwrap();
        assert_eq!(rejected.state, VerificationState::Rejected);
        assert_eq!(rejected.pipeline_id, None);
    }

    #[test]
    fn product_only_changes_continue_to_quarantine() {
        let directory = tempfile::tempdir().unwrap();
        let ledger = Ledger::new(directory.path()).unwrap();
        let entry = ledger.reserve("head", "base", Scope::Full).unwrap();

        assert!(
            !reject_control_changes(
                427,
                "head",
                &entry,
                &[],
                |_, _, _, _| panic!("product-only changes must not report a policy failure"),
                |_, _| panic!("product-only changes must not be rejected"),
            )
            .unwrap()
        );
    }

    #[test]
    fn a_merged_github_head_is_only_fast_forwarded() {
        let github_sha = "0123456789abcdef0123456789abcdef01234567";
        let gitlab_sha = "89abcdef0123456789abcdef0123456789abcdef";
        let actions = Rc::new(RefCell::new(Vec::new()));

        fast_forward_github_import(
            github_sha,
            gitlab_sha,
            Branches {
                github: "main",
                gitlab: "develop",
            },
            {
                let actions = Rc::clone(&actions);
                move |_| {
                    actions.borrow_mut().push(ImportAction::CheckProvenance);
                    Ok(Some(427))
                }
            },
            {
                let actions = Rc::clone(&actions);
                move || {
                    actions.borrow_mut().push(ImportAction::RefreshHeads);
                    Ok((github_sha.to_owned(), gitlab_sha.to_owned()))
                }
            },
            |_| panic!("a merged pull request must not open an incident"),
            {
                let actions = Rc::clone(&actions);
                move |sha, branch| {
                    actions
                        .borrow_mut()
                        .push(ImportAction::Push(format!("{sha}:{branch}")));
                    Ok(())
                }
            },
        )
        .unwrap();

        assert_eq!(
            *actions.borrow(),
            [
                ImportAction::CheckProvenance,
                ImportAction::RefreshHeads,
                ImportAction::Push(format!("{github_sha}:develop")),
            ]
        );
    }

    #[test]
    fn changed_heads_stop_the_import_before_the_push() {
        let github_sha = "0123456789abcdef0123456789abcdef01234567";
        let gitlab_sha = "89abcdef0123456789abcdef0123456789abcdef";

        let error = fast_forward_github_import(
            github_sha,
            gitlab_sha,
            Branches {
                github: "main",
                gitlab: "develop",
            },
            |_| Ok(Some(427)),
            || {
                Ok((
                    "fedcba9876543210fedcba9876543210fedcba98".into(),
                    gitlab_sha.into(),
                ))
            },
            |_| panic!("a merged pull request must not open an incident"),
            |_, _| panic!("a stale observation must not be pushed"),
        )
        .unwrap_err();

        assert!(error.to_string().contains("heads changed"));
    }

    #[test]
    fn a_direct_github_update_opens_an_incident_without_a_push() {
        let github_sha = "0123456789abcdef0123456789abcdef01234567";
        let detail = Rc::new(RefCell::new(None));

        let error = fast_forward_github_import(
            github_sha,
            "89abcdef0123456789abcdef0123456789abcdef",
            Branches {
                github: "main",
                gitlab: "develop",
            },
            |_| Ok(None),
            || panic!("an untrusted head must not refresh for promotion"),
            {
                let detail = Rc::clone(&detail);
                move |message| {
                    *detail.borrow_mut() = Some(message.to_owned());
                    Ok(())
                }
            },
            |_, _| panic!("an untrusted head must not be pushed"),
        )
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("not associated with a merged pull request")
        );
        assert_eq!(
            detail.borrow().as_deref(),
            Some(
                "GitHub head 0123456789abcdef0123456789abcdef01234567 is not associated with a \
             merged pull request targeting main"
            )
        );
    }

    /// A conflict is a verdict, and it has to reach the author as one. Left as an
    /// error it would only retry each minute forever, with the pull request sitting
    /// at "verification running" and nothing to act on.
    #[test]
    fn an_unmergeable_head_is_failed_and_rejected_with_the_reason() {
        let directory = tempfile::tempdir().unwrap();
        let ledger = Ledger::new(directory.path()).unwrap();
        let entry = ledger.reserve("head", "base", Scope::Full).unwrap();
        let actions = RefCell::new(Vec::new());

        reject_unmergeable(
            118,
            "head",
            "base",
            &entry,
            |sha, state, detail, _| {
                assert!(detail.contains("does not merge"), "{detail}");
                assert!(detail.contains("Merge the default branch"), "{detail}");
                actions
                    .borrow_mut()
                    .push(VerificationAction::Report(sha.into(), state.into()));
                Ok(())
            },
            |attempt, _| {
                actions
                    .borrow_mut()
                    .push(VerificationAction::Reject(attempt));
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(
            *actions.borrow(),
            [
                VerificationAction::Report("head".into(), "failure".into()),
                VerificationAction::Reject(entry.attempt),
            ]
        );
    }

    fn open_pulls() -> Vec<LiveVerification> {
        vec![verification(consts::CURRENT_BASE, Scope::Full)]
    }

    fn verification(base: &str, scope: Scope) -> LiveVerification {
        LiveVerification {
            head_sha: consts::PULL_HEAD.to_owned(),
            base_sha: base.to_owned(),
            scope,
        }
    }

    #[test]
    fn a_drafts_branch_is_named_apart_from_a_full_run() {
        assert_eq!(
            quarantine_ref("head", "base", 1, Scope::Draft),
            "quarantine/gh/head-base/draft-attempt-1"
        );
        assert_ne!(
            quarantine_ref("head", "base", 1, Scope::Draft),
            quarantine_ref("head", "base", 1, Scope::Full)
        );
    }

    /// The default branch moving re-queued every open pull request on the one
    /// Mac host, drafts included. A draft stays where it was judged until its
    /// author pushes.
    #[test]
    fn a_draft_stays_on_the_base_it_was_judged_against() {
        assert_eq!(
            verification_base(
                Scope::Draft,
                Some(consts::OLDER_BASE.into()),
                consts::CURRENT_BASE
            ),
            consts::OLDER_BASE
        );
        assert_eq!(
            verification_base(Scope::Draft, None, consts::CURRENT_BASE),
            consts::CURRENT_BASE
        );
        assert_eq!(
            verification_base(
                Scope::Full,
                Some(consts::OLDER_BASE.into()),
                consts::CURRENT_BASE
            ),
            consts::CURRENT_BASE
        );
    }

    #[test]
    fn a_drafts_run_against_its_own_base_survives_a_moved_default_branch() {
        let refs = [quarantine_ref(
            consts::PULL_HEAD,
            consts::OLDER_BASE,
            1,
            Scope::Draft,
        )];

        assert_eq!(
            superseded_quarantine_refs(&refs, &[verification(consts::OLDER_BASE, Scope::Draft)]),
            [] as [&str; 0]
        );
    }

    /// A draft marked ready is judged in full; the narrower run it had is
    /// stopped rather than left holding the Mac.
    #[test]
    fn a_drafts_run_is_superseded_once_it_is_ready() {
        let refs = [quarantine_ref(
            consts::PULL_HEAD,
            consts::CURRENT_BASE,
            1,
            Scope::Draft,
        )];

        assert_eq!(
            superseded_quarantine_refs(&refs, &open_pulls()),
            [refs[0].as_str()]
        );
    }

    #[test]
    fn a_draft_reports_that_it_ran_only_what_it_touched() {
        assert_eq!(scoped(Scope::Full, "passed".into()), "passed");
        assert!(scoped(Scope::Draft, "passed".into()).starts_with("Draft, touched tests only:"));
    }

    #[test]
    fn a_quarantine_ref_judged_against_an_older_base_is_superseded() {
        let refs = [quarantine_ref(
            consts::PULL_HEAD,
            consts::OLDER_BASE,
            1,
            Scope::Full,
        )];

        assert_eq!(
            superseded_quarantine_refs(&refs, &open_pulls()),
            [refs[0].as_str()]
        );
    }

    #[test]
    fn a_quarantine_ref_judged_against_the_current_base_is_kept() {
        let refs = [quarantine_ref(
            consts::PULL_HEAD,
            consts::CURRENT_BASE,
            2,
            Scope::Full,
        )];

        assert_eq!(
            superseded_quarantine_refs(&refs, &open_pulls()),
            [] as [&str; 0]
        );
    }

    #[test]
    fn a_branch_that_is_not_a_verification_run_is_never_swept() {
        let refs = ["develop".to_owned(), "laba/419-connectivity".to_owned()];

        assert_eq!(
            superseded_quarantine_refs(&refs, &open_pulls()),
            [] as [&str; 0]
        );
    }

    #[test]
    fn the_older_quarantine_naming_scheme_is_swept_by_the_same_rule() {
        let refs = [format!(
            "quarantine/github/{PULL_HEAD}/{OLDER_BASE}/attempt-1",
            OLDER_BASE = consts::OLDER_BASE,
            PULL_HEAD = consts::PULL_HEAD
        )];

        assert_eq!(
            superseded_quarantine_refs(&refs, &open_pulls()),
            [refs[0].as_str()]
        );
    }

    #[test]
    fn a_sweep_drops_every_branch_a_moved_base_left_behind() {
        let stale = quarantine_ref(consts::PULL_HEAD, consts::OLDER_BASE, 1, Scope::Full);
        let live = quarantine_ref(consts::PULL_HEAD, consts::CURRENT_BASE, 2, Scope::Full);
        let listed = vec![stale.clone(), live, "develop".to_owned()];
        let deleted = RefCell::new(Vec::new());

        sweep_quarantine_refs(
            &open_pulls(),
            || Ok(listed),
            |_| Ok(()),
            |refs| {
                deleted
                    .borrow_mut()
                    .extend(refs.iter().map(|reference| (*reference).to_owned()));
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(*deleted.borrow(), [stale]);
    }

    #[test]
    fn a_sweep_with_nothing_left_behind_never_reaches_the_remote() {
        let called = Cell::new(false);

        sweep_quarantine_refs(
            &open_pulls(),
            || {
                Ok(vec![quarantine_ref(
                    consts::PULL_HEAD,
                    consts::CURRENT_BASE,
                    1,
                    Scope::Full,
                )])
            },
            |_| Ok(()),
            |_| {
                called.set(true);
                Ok(())
            },
        )
        .unwrap();

        assert!(!called.get());
    }

    /// The queue this leaves behind. A pull request that gets a new commit keeps
    /// its previous verification standing in the resource group, and the mac
    /// runner takes one job at a time: nine of these were waiting at once, ahead
    /// of the runs people were waiting on. The base rule cannot see them, because
    /// the base did not move — the head did.
    #[test]
    fn a_quarantine_ref_for_a_head_no_open_pull_request_names_is_superseded() {
        let refs = [quarantine_ref(
            consts::RETIRED_HEAD,
            consts::CURRENT_BASE,
            1,
            Scope::Full,
        )];

        assert_eq!(
            superseded_quarantine_refs(&refs, &open_pulls()),
            [refs[0].as_str()]
        );
    }

    #[test]
    fn a_quarantine_ref_for_an_open_pull_requests_head_is_kept() {
        let refs = [quarantine_ref(
            consts::PULL_HEAD,
            consts::CURRENT_BASE,
            1,
            Scope::Full,
        )];

        assert_eq!(
            superseded_quarantine_refs(&refs, &open_pulls()),
            [] as [&str; 0]
        );
    }

    /// Deleting the branch does not stop its run: `GitLab` keeps a queued pipeline
    /// in the resource group after the ref it names is gone, so the slot stays
    /// taken by a commit nobody will merge.
    #[test]
    fn a_sweep_cancels_the_run_of_every_branch_it_drops() {
        let stale = quarantine_ref(consts::RETIRED_HEAD, consts::CURRENT_BASE, 1, Scope::Full);
        let live = quarantine_ref(consts::PULL_HEAD, consts::CURRENT_BASE, 1, Scope::Full);
        let listed = vec![stale.clone(), live, "develop".to_owned()];
        let cancelled = RefCell::new(Vec::new());

        sweep_quarantine_refs(
            &open_pulls(),
            || Ok(listed),
            |reference| {
                cancelled.borrow_mut().push(reference.to_owned());
                Ok(())
            },
            |_| Ok(()),
        )
        .unwrap();

        assert_eq!(*cancelled.borrow(), [stale]);
    }
}

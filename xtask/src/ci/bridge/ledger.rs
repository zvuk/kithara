use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use kithara_devtools::lock::{FileLock, Wait};
use serde::{Deserialize, Serialize};

use super::model::{Scope, VerificationState};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LedgerEntry {
    pub(super) state: VerificationState,
    pub(super) attempt: u64,
    pub(super) pipeline_id: Option<u64>,
    pub(super) announced: bool,
    pub(super) detail: Option<String>,
    /// Entries written before drafts were scoped were all full verifications.
    #[serde(default)]
    pub(super) scope: Scope,
    updated_at: u64,
}

/// What the bridge last posted on one default-branch commit.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DefaultBranchStatus {
    pub(super) pipeline_id: u64,
    pub(super) state: String,
    pub(super) description: String,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LedgerData {
    verifications: BTreeMap<String, LedgerEntry>,
    /// Older ledgers have no such field, meaning nothing has been posted yet.
    #[serde(default)]
    default_branch: BTreeMap<String, DefaultBranchStatus>,
}

pub(super) struct Ledger {
    path: PathBuf,
    lock_path: PathBuf,
}

impl Ledger {
    pub(super) fn new(state_dir: &Path) -> Result<Self> {
        fs::create_dir_all(state_dir)
            .with_context(|| format!("creating bridge state {}", state_dir.display()))?;
        Ok(Self {
            path: state_dir.join("verification-ledger.json"),
            lock_path: state_dir.join("verification-ledger.lock"),
        })
    }

    pub(super) fn default_branch_statuses(&self) -> Result<BTreeMap<String, DefaultBranchStatus>> {
        self.with_locked_data(|data| Ok(data.default_branch.clone()))
    }

    pub(super) fn record_default_branch(
        &self,
        sha: &str,
        status: DefaultBranchStatus,
    ) -> Result<()> {
        self.with_locked_data(|data| {
            data.default_branch.insert(sha.to_owned(), status);
            self.write(data)
        })
    }

    pub(super) fn forget_default_branch(&self, sha: &str) -> Result<()> {
        self.with_locked_data(|data| {
            data.default_branch.remove(sha);
            self.write(data)
        })
    }

    pub(super) fn get(&self, head_sha: &str, base_sha: &str) -> Result<Option<LedgerEntry>> {
        self.with_locked_data(|data| {
            Ok(data
                .verifications
                .get(&ledger_key(head_sha, base_sha))
                .cloned())
        })
    }

    /// The most recently updated pass for this head against a different base.
    /// Equal timestamps choose the lexicographically later ledger key.
    pub(super) fn last_pass(&self, head_sha: &str, base_sha: &str) -> Result<Option<String>> {
        self.with_locked_data(|data| {
            Ok(data
                .verifications
                .iter()
                .filter(|(_, entry)| {
                    entry.state == VerificationState::Verified && entry.scope == Scope::Full
                })
                .filter_map(|(key, entry)| {
                    let (head, base) = key.split_once(':')?;
                    (head == head_sha && base != base_sha).then_some((entry.updated_at, key, base))
                })
                .max_by_key(|(updated_at, key, _)| (*updated_at, *key))
                .map(|(_, _, base)| base.to_owned()))
        })
    }

    /// The base of the verification of this head that changed last, whatever
    /// its scope or state. Equal timestamps choose the later ledger key.
    pub(super) fn latest_base(
        &self,
        head_sha: &str,
    ) -> Result<Option<(String, VerificationState)>> {
        self.with_locked_data(|data| {
            Ok(data
                .verifications
                .iter()
                .filter_map(|(key, entry)| {
                    let (head, base) = key.split_once(':')?;
                    (head == head_sha).then_some((entry.updated_at, key, base, entry.state))
                })
                .max_by_key(|(updated_at, key, _, _)| (*updated_at, *key))
                .map(|(_, _, base, state)| (base.to_owned(), state)))
        })
    }

    /// Whether reserving this head against this base in `scope` turns a
    /// draft's verification into a full one.
    pub(super) fn turns_ready(&self, head_sha: &str, base_sha: &str, scope: Scope) -> Result<bool> {
        self.with_locked_data(|data| {
            Ok(scope == Scope::Full
                && data
                    .verifications
                    .get(&ledger_key(head_sha, base_sha))
                    .is_some_and(|entry| entry.scope == Scope::Draft))
        })
    }

    /// The verification of this head against this base, opened when there is
    /// none. A full verification answers for a draft; a draft that turned
    /// ready opens its next attempt as a full one.
    pub(super) fn reserve(
        &self,
        head_sha: &str,
        base_sha: &str,
        scope: Scope,
    ) -> Result<LedgerEntry> {
        self.with_locked_data(|data| {
            let key = ledger_key(head_sha, base_sha);
            if let Some(entry) = data.verifications.get_mut(&key) {
                if entry.scope == Scope::Draft && scope == Scope::Full {
                    reopen(entry)?;
                    entry.scope = Scope::Full;
                    let entry = entry.clone();
                    self.write(data)?;
                    return Ok(entry);
                }
                return Ok(entry.clone());
            }
            let entry = LedgerEntry {
                state: VerificationState::Testing,
                attempt: 1,
                pipeline_id: None,
                announced: false,
                detail: None,
                scope,
                updated_at: unix_time()?,
            };
            data.verifications.insert(key, entry.clone());
            self.write(data)?;
            Ok(entry)
        })
    }

    pub(super) fn attach(
        &self,
        head_sha: &str,
        base_sha: &str,
        attempt: u64,
        pipeline_id: u64,
    ) -> Result<()> {
        self.update(head_sha, base_sha, |data, key| {
            let entry = testing_attempt(data, key, attempt)?;
            match entry.pipeline_id {
                Some(previous) if previous != pipeline_id => bail!(
                    "verification {key} attempt {attempt} owns pipeline {previous}; refusing {pipeline_id}"
                ),
                Some(_) => Ok(false),
                None => {
                    entry.pipeline_id = Some(pipeline_id);
                    Ok(true)
                }
            }
        })
    }

    pub(super) fn announce(
        &self,
        head_sha: &str,
        base_sha: &str,
        attempt: u64,
        pipeline_id: u64,
    ) -> Result<()> {
        self.update(head_sha, base_sha, |data, key| {
            let entry = owned_pipeline(data, key, attempt, pipeline_id)?;
            if entry.announced {
                return Ok(false);
            }
            entry.announced = true;
            Ok(true)
        })
    }

    fn update(
        &self,
        head_sha: &str,
        base_sha: &str,
        change: impl FnOnce(&mut LedgerData, &str) -> Result<bool>,
    ) -> Result<()> {
        self.with_locked_data(|data| {
            let key = ledger_key(head_sha, base_sha);
            if !change(data, &key)? {
                return Ok(());
            }
            data.verifications
                .get_mut(&key)
                .ok_or_else(|| anyhow::anyhow!("verification {key} has not been reserved"))?
                .updated_at = unix_time()?;
            self.write(data)
        })
    }

    pub(super) fn finish(
        &self,
        head_sha: &str,
        base_sha: &str,
        attempt: u64,
        pipeline_id: u64,
        state: VerificationState,
        detail: Option<String>,
    ) -> Result<()> {
        self.with_locked_data(|data| {
            let key = ledger_key(head_sha, base_sha);
            let entry = owned_pipeline(data, &key, attempt, pipeline_id)?;
            if entry.state == state && entry.detail == detail {
                return Ok(());
            }
            entry.state = state;
            entry.detail = detail;
            entry.updated_at = unix_time()?;
            self.write(data)
        })
    }

    pub(super) fn reject(
        &self,
        head_sha: &str,
        base_sha: &str,
        attempt: u64,
        detail: String,
    ) -> Result<()> {
        self.with_locked_data(|data| {
            let key = ledger_key(head_sha, base_sha);
            let Some(entry) = data.verifications.get_mut(&key) else {
                bail!("verification {key} has not been reserved");
            };
            require_attempt(entry, &key, attempt)?;
            if entry.state != VerificationState::Testing {
                return Ok(());
            }
            entry.state = VerificationState::Rejected;
            entry.detail = Some(detail);
            entry.updated_at = unix_time()?;
            self.write(data)
        })
    }

    pub(super) fn retry(&self, head_sha: &str, base_sha: &str) -> Result<LedgerEntry> {
        self.with_locked_data(|data| {
            let key = ledger_key(head_sha, base_sha);
            let Some(entry) = data.verifications.get_mut(&key) else {
                bail!("no verification recorded for {key}");
            };
            if entry.state == VerificationState::Testing {
                bail!("verification {key} is still testing");
            }
            reopen(entry)?;
            let retry = entry.clone();
            self.write(data)?;
            Ok(retry)
        })
    }

    /// Give a verification its next attempt after the run it owned was stopped.
    ///
    /// A cancelled pipeline reports nothing, so there is nothing to record and
    /// nothing to wait for: the pipeline it names will never move again, and
    /// the entry would sit on it until someone noticed. The attempt advances
    /// because the branch a verification runs on is named after it, and the
    /// pipeline query that recovers a lost create would otherwise hand back the
    /// stopped run.
    pub(super) fn release(
        &self,
        head_sha: &str,
        base_sha: &str,
        attempt: u64,
        pipeline_id: u64,
    ) -> Result<()> {
        self.with_locked_data(|data| {
            let key = ledger_key(head_sha, base_sha);
            let entry = owned_pipeline(data, &key, attempt, pipeline_id)?;
            reopen(entry)?;
            self.write(data)
        })
    }

    fn with_locked_data<T>(
        &self,
        operation: impl FnOnce(&mut LedgerData) -> Result<T>,
    ) -> Result<T> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&self.lock_path)
            .with_context(|| format!("opening ledger lock {}", self.lock_path.display()))?;
        let subject = format!("bridge ledger {}", self.lock_path.display());
        let holder = crate::job::lock_holder();
        let _lock = FileLock::exclusive(
            file,
            &Wait {
                subject: &subject,
                holder: &holder,
            },
        )
        .with_context(|| format!("locking {subject}"))?;
        let mut data = self.read()?;
        operation(&mut data)
    }

    fn read(&self) -> Result<LedgerData> {
        if !self.path.exists() {
            return Ok(LedgerData::default());
        }
        let text = fs::read_to_string(&self.path)
            .with_context(|| format!("reading bridge ledger {}", self.path.display()))?;
        serde_json::from_str(&text)
            .with_context(|| format!("parsing bridge ledger {}", self.path.display()))
    }

    fn write(&self, data: &LedgerData) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(data).context("serializing bridge ledger")?;
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("system clock is before Unix epoch")?
            .as_nanos();
        let temporary =
            self.path
                .with_extension(format!("json.{}.{}.tmp", std::process::id(), suffix));
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .with_context(|| format!("creating temporary ledger {}", temporary.display()))?;
        file.write_all(&bytes)
            .with_context(|| format!("writing temporary ledger {}", temporary.display()))?;
        file.write_all(b"\n")
            .with_context(|| format!("terminating temporary ledger {}", temporary.display()))?;
        file.sync_all()
            .with_context(|| format!("syncing temporary ledger {}", temporary.display()))?;
        drop(file);
        fs::rename(&temporary, &self.path).with_context(|| {
            format!(
                "publishing bridge ledger {} as {}",
                temporary.display(),
                self.path.display()
            )
        })?;
        let parent = self
            .path
            .parent()
            .context("verification ledger has no parent directory")?;
        fs::File::open(parent)
            .with_context(|| format!("opening ledger directory {}", parent.display()))?
            .sync_all()
            .with_context(|| format!("syncing ledger directory {}", parent.display()))
    }
}

fn testing_attempt<'a>(
    data: &'a mut LedgerData,
    key: &str,
    attempt: u64,
) -> Result<&'a mut LedgerEntry> {
    let Some(entry) = data.verifications.get_mut(key) else {
        bail!("verification {key} has not been reserved");
    };
    require_attempt(entry, key, attempt)?;
    if entry.state != VerificationState::Testing {
        bail!("verification {key} attempt {attempt} is terminal");
    }
    Ok(entry)
}

/// Open the next attempt on an entry, forgetting the run the last one owned.
fn reopen(entry: &mut LedgerEntry) -> Result<()> {
    entry.attempt = entry
        .attempt
        .checked_add(1)
        .context("verification attempt counter overflowed")?;
    entry.state = VerificationState::Testing;
    entry.pipeline_id = None;
    entry.announced = false;
    entry.detail = None;
    entry.updated_at = unix_time()?;
    Ok(())
}

fn require_attempt(entry: &LedgerEntry, key: &str, attempt: u64) -> Result<()> {
    if entry.attempt != attempt {
        bail!(
            "verification {key} is on attempt {}; refusing stale attempt {attempt}",
            entry.attempt
        );
    }
    Ok(())
}

fn owned_pipeline<'a>(
    data: &'a mut LedgerData,
    key: &str,
    attempt: u64,
    pipeline_id: u64,
) -> Result<&'a mut LedgerEntry> {
    let Some(entry) = data.verifications.get_mut(key) else {
        bail!("verification {key} has not been reserved");
    };
    require_attempt(entry, key, attempt)?;
    if entry.pipeline_id != Some(pipeline_id) {
        bail!("verification {key} attempt {attempt} does not own pipeline {pipeline_id}");
    }
    Ok(entry)
}

fn ledger_key(head_sha: &str, base_sha: &str) -> String {
    format!("{head_sha}:{base_sha}")
}

fn unix_time() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before Unix epoch")?
        .as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ci::bridge::model::Scope;

    #[test]
    fn default_branch_statuses_round_trip() {
        let directory = tempfile::tempdir().unwrap();
        let ledger = Ledger::new(directory.path()).unwrap();
        let pending = DefaultBranchStatus {
            pipeline_id: 1,
            state: "pending".into(),
            description: "GitLab pipeline 1 on stable running".into(),
        };
        let passed = DefaultBranchStatus {
            pipeline_id: 2,
            state: "success".into(),
            description: "GitLab pipeline 2 on stable passed".into(),
        };

        ledger.record_default_branch("a", pending).unwrap();
        ledger.record_default_branch("b", passed.clone()).unwrap();
        ledger.forget_default_branch("a").unwrap();

        let reopened = Ledger::new(directory.path()).unwrap();
        assert_eq!(
            reopened.default_branch_statuses().unwrap(),
            BTreeMap::from([("b".into(), passed)])
        );
    }

    #[test]
    fn a_ledger_without_default_branch_statuses_loads_empty() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join("verification-ledger.json"),
            r#"{"verifications":{}}"#,
        )
        .unwrap();
        let ledger = Ledger::new(directory.path()).unwrap();
        assert!(ledger.read().unwrap().default_branch.is_empty());
        assert!(ledger.default_branch_statuses().unwrap().is_empty());
        let pending = DefaultBranchStatus {
            pipeline_id: 1,
            state: "pending".into(),
            description: "GitLab pipeline 1 on stable running".into(),
        };

        ledger.record_default_branch("a", pending.clone()).unwrap();

        assert_eq!(
            ledger.default_branch_statuses().unwrap(),
            BTreeMap::from([("a".into(), pending)])
        );
    }

    fn finish_entry(ledger: &Ledger, head: &str, base: &str, state: VerificationState) {
        let entry = ledger.reserve(head, base, Scope::Full).unwrap();
        ledger.attach(head, base, entry.attempt, 42).unwrap();
        ledger
            .finish(head, base, entry.attempt, 42, state, None)
            .unwrap();
    }

    #[test]
    fn a_draft_that_turns_ready_is_reopened_as_a_full_verification() {
        let directory = tempfile::tempdir().unwrap();
        let ledger = Ledger::new(directory.path()).unwrap();
        let draft = ledger.reserve("head", "base", Scope::Draft).unwrap();
        ledger.attach("head", "base", draft.attempt, 42).unwrap();
        ledger
            .finish(
                "head",
                "base",
                draft.attempt,
                42,
                VerificationState::Verified,
                None,
            )
            .unwrap();

        let full = ledger.reserve("head", "base", Scope::Full).unwrap();

        assert_eq!(full.scope, Scope::Full);
        assert_eq!(full.state, VerificationState::Testing);
        assert_eq!(full.attempt, draft.attempt + 1);
        assert_eq!(full.pipeline_id, None);
    }

    #[test]
    fn only_a_draft_reserved_as_full_turns_ready() {
        let directory = tempfile::tempdir().unwrap();
        let ledger = Ledger::new(directory.path()).unwrap();

        assert!(!ledger.turns_ready("head", "base", Scope::Full).unwrap());
        ledger.reserve("head", "base", Scope::Draft).unwrap();
        assert!(!ledger.turns_ready("head", "base", Scope::Draft).unwrap());
        assert!(ledger.turns_ready("head", "base", Scope::Full).unwrap());
        ledger.reserve("head", "base", Scope::Full).unwrap();
        assert!(!ledger.turns_ready("head", "base", Scope::Full).unwrap());
    }

    #[test]
    fn a_full_verification_answers_for_a_draft() {
        let directory = tempfile::tempdir().unwrap();
        let ledger = Ledger::new(directory.path()).unwrap();
        let full = ledger.reserve("head", "base", Scope::Full).unwrap();

        let draft = ledger.reserve("head", "base", Scope::Draft).unwrap();

        assert_eq!(draft, full);
    }

    #[test]
    fn a_draft_pass_is_no_pass_of_the_merge() {
        let directory = tempfile::tempdir().unwrap();
        let ledger = Ledger::new(directory.path()).unwrap();
        let draft = ledger.reserve("head", "base-one", Scope::Draft).unwrap();
        ledger
            .attach("head", "base-one", draft.attempt, 42)
            .unwrap();
        ledger
            .finish(
                "head",
                "base-one",
                draft.attempt,
                42,
                VerificationState::Verified,
                None,
            )
            .unwrap();

        assert_eq!(ledger.last_pass("head", "base-current").unwrap(), None);
    }

    #[test]
    fn a_head_stands_on_the_base_it_was_last_verified_against() {
        let directory = tempfile::tempdir().unwrap();
        let ledger = Ledger::new(directory.path()).unwrap();
        assert_eq!(ledger.latest_base("head").unwrap(), None);
        ledger.reserve("head", "base-one", Scope::Draft).unwrap();
        ledger.reserve("other", "base-two", Scope::Draft).unwrap();

        assert_eq!(
            ledger.latest_base("head").unwrap(),
            Some(("base-one".into(), VerificationState::Testing))
        );
    }

    #[test]
    fn last_pass_selects_the_latest_of_two_older_verified_bases() {
        let directory = tempfile::tempdir().unwrap();
        let ledger = Ledger::new(directory.path()).unwrap();
        finish_entry(&ledger, "head", "base-one", VerificationState::Verified);
        finish_entry(&ledger, "head", "base-two", VerificationState::Verified);

        assert_eq!(
            ledger.last_pass("head", "base-current").unwrap(),
            Some("base-two".into())
        );
    }

    #[test]
    fn last_pass_ignores_a_later_rejected_entry() {
        let directory = tempfile::tempdir().unwrap();
        let ledger = Ledger::new(directory.path()).unwrap();
        finish_entry(&ledger, "head", "base-one", VerificationState::Verified);
        finish_entry(&ledger, "head", "base-two", VerificationState::Rejected);

        assert_eq!(
            ledger.last_pass("head", "base-current").unwrap(),
            Some("base-one".into())
        );
    }

    #[test]
    fn last_pass_ignores_the_current_verified_base() {
        let directory = tempfile::tempdir().unwrap();
        let ledger = Ledger::new(directory.path()).unwrap();
        finish_entry(&ledger, "head", "base-one", VerificationState::Verified);
        finish_entry(&ledger, "head", "base-two", VerificationState::Verified);

        assert_eq!(
            ledger.last_pass("head", "base-two").unwrap(),
            Some("base-one".into())
        );
    }

    #[test]
    fn last_pass_ignores_another_heads_later_verified_entry() {
        let directory = tempfile::tempdir().unwrap();
        let ledger = Ledger::new(directory.path()).unwrap();
        finish_entry(&ledger, "head-one", "base-one", VerificationState::Verified);
        finish_entry(&ledger, "head-two", "base-two", VerificationState::Verified);

        assert_eq!(
            ledger.last_pass("head-one", "base-current").unwrap(),
            Some("base-one".into())
        );
    }

    #[test]
    fn last_pass_ignores_a_later_testing_entry() {
        let directory = tempfile::tempdir().unwrap();
        let ledger = Ledger::new(directory.path()).unwrap();
        finish_entry(&ledger, "head", "base-one", VerificationState::Verified);
        ledger.reserve("head", "base-two", Scope::Full).unwrap();

        assert_eq!(
            ledger.last_pass("head", "base-current").unwrap(),
            Some("base-one".into())
        );
    }

    #[test]
    fn exact_head_and_base_reuse_one_durable_reservation() {
        let directory = tempfile::tempdir().unwrap();
        let ledger = Ledger::new(directory.path()).unwrap();

        let first = ledger.reserve("head", "base", Scope::Full).unwrap();
        let second = ledger.reserve("head", "base", Scope::Full).unwrap();

        assert_eq!(first, second);
        assert_eq!(first.attempt, 1);
        assert_eq!(first.pipeline_id, None);
    }

    #[test]
    fn changed_head_or_base_is_a_new_verification_key() {
        let directory = tempfile::tempdir().unwrap();
        let ledger = Ledger::new(directory.path()).unwrap();
        ledger.reserve("head-one", "base-one", Scope::Full).unwrap();
        ledger.reserve("head-two", "base-one", Scope::Full).unwrap();
        ledger.reserve("head-one", "base-two", Scope::Full).unwrap();

        assert_eq!(
            ledger.get("head-two", "base-one").unwrap().unwrap().attempt,
            1
        );
        assert_eq!(
            ledger.get("head-one", "base-two").unwrap().unwrap().attempt,
            1
        );
    }

    #[test]
    fn job_retries_refresh_the_owned_pipeline_without_changing_its_identity() {
        let directory = tempfile::tempdir().unwrap();
        let ledger = Ledger::new(directory.path()).unwrap();
        ledger.reserve("head", "base", Scope::Full).unwrap();
        ledger.attach("head", "base", 1, 42).unwrap();
        ledger.announce("head", "base", 1, 42).unwrap();
        for state in [
            VerificationState::Rejected,
            VerificationState::Testing,
            VerificationState::Verified,
            VerificationState::Testing,
            VerificationState::Rejected,
        ] {
            ledger.finish("head", "base", 1, 42, state, None).unwrap();
            let entry = ledger.get("head", "base").unwrap().unwrap();
            assert_eq!(entry.state, state);
            assert_eq!(entry.attempt, 1);
            assert_eq!(entry.pipeline_id, Some(42));
            assert!(entry.announced);
        }
        assert!(
            ledger
                .finish("head", "base", 1, 43, VerificationState::Verified, None)
                .is_err()
        );
        assert!(
            ledger
                .finish("head", "base", 2, 42, VerificationState::Verified, None)
                .is_err()
        );
        assert!(ledger.attach("head", "base", 1, 43).is_err());
        ledger.release("head", "base", 1, 42).unwrap();
        assert_eq!(ledger.get("head", "base").unwrap().unwrap().attempt, 2);
    }

    #[test]
    fn retry_requires_the_exact_terminal_key() {
        let directory = tempfile::tempdir().unwrap();
        let ledger = Ledger::new(directory.path()).unwrap();
        ledger.reserve("head", "base", Scope::Full).unwrap();
        ledger.attach("head", "base", 1, 42).unwrap();
        assert!(ledger.retry("head", "base").is_err());
        ledger
            .finish("head", "base", 1, 42, VerificationState::Rejected, None)
            .unwrap();
        assert!(ledger.retry("head", "other-base").is_err());
        let retry = ledger.retry("head", "base").unwrap();
        assert_eq!(retry.attempt, 2);
        assert_eq!(retry.pipeline_id, None);
        assert_eq!(ledger.get("head", "base").unwrap().unwrap().attempt, 2);
    }

    /// A run someone stopped is released, not recorded. The verification stays
    /// open on a fresh attempt, so the next tick pushes a branch of its own
    /// instead of reading a pipeline that will never report again.
    #[test]
    fn releasing_a_stopped_pipeline_keeps_the_verification_open_on_a_fresh_attempt() {
        let directory = tempfile::tempdir().unwrap();
        let ledger = Ledger::new(directory.path()).unwrap();
        ledger.reserve("head", "base", Scope::Full).unwrap();
        ledger.attach("head", "base", 1, 42).unwrap();
        ledger.announce("head", "base", 1, 42).unwrap();

        assert!(ledger.release("head", "base", 1, 43).is_err());
        ledger.release("head", "base", 1, 42).unwrap();

        let entry = ledger.get("head", "base").unwrap().unwrap();
        assert_eq!(entry.state, VerificationState::Testing);
        assert_eq!(entry.attempt, 2);
        assert_eq!(entry.pipeline_id, None);
        assert!(!entry.announced);
        assert_eq!(entry.detail, None);
    }

    #[test]
    fn policy_rejection_is_terminal_without_a_pipeline() {
        let directory = tempfile::tempdir().unwrap();
        let ledger = Ledger::new(directory.path()).unwrap();
        let entry = ledger.reserve("head", "base", Scope::Full).unwrap();

        ledger
            .reject("head", "base", entry.attempt, "control path changed".into())
            .unwrap();

        let rejected = ledger.get("head", "base").unwrap().unwrap();
        assert_eq!(rejected.state, VerificationState::Rejected);
        assert_eq!(rejected.pipeline_id, None);
        assert_eq!(rejected.detail.as_deref(), Some("control path changed"));
    }
}

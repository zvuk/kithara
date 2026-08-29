use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use kithara_devtools::lock::FileLock;
use serde::{Deserialize, Serialize};

use super::model::VerificationState;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LedgerEntry {
    pub(super) state: VerificationState,
    pub(super) attempt: u64,
    pub(super) pipeline_id: Option<u64>,
    pub(super) announced: bool,
    pub(super) detail: Option<String>,
    updated_at: u64,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LedgerData {
    verifications: BTreeMap<String, LedgerEntry>,
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

    #[cfg(test)]
    pub(super) fn get(&self, head_sha: &str, base_sha: &str) -> Result<Option<LedgerEntry>> {
        self.with_locked_data(|data| {
            Ok(data
                .verifications
                .get(&ledger_key(head_sha, base_sha))
                .cloned())
        })
    }

    pub(super) fn reserve(&self, head_sha: &str, base_sha: &str) -> Result<LedgerEntry> {
        self.with_locked_data(|data| {
            let key = ledger_key(head_sha, base_sha);
            if let Some(entry) = data.verifications.get(&key) {
                return Ok(entry.clone());
            }
            let entry = LedgerEntry {
                state: VerificationState::Testing,
                attempt: 1,
                pipeline_id: None,
                announced: false,
                detail: None,
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
        self.with_locked_data(|data| {
            let key = ledger_key(head_sha, base_sha);
            let entry = testing_attempt(data, &key, attempt)?;
            match entry.pipeline_id {
                Some(previous) if previous != pipeline_id => bail!(
                    "verification {key} attempt {attempt} owns pipeline {previous}; refusing {pipeline_id}"
                ),
                Some(_) => Ok(()),
                None => {
                    entry.pipeline_id = Some(pipeline_id);
                    entry.updated_at = unix_time()?;
                    self.write(data)
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
        self.with_locked_data(|data| {
            let key = ledger_key(head_sha, base_sha);
            let entry = testing_pipeline(data, &key, attempt, pipeline_id)?;
            if entry.announced {
                return Ok(());
            }
            entry.announced = true;
            entry.updated_at = unix_time()?;
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
            let Some(entry) = data.verifications.get_mut(&key) else {
                bail!("verification {key} has not been reserved");
            };
            require_attempt(entry, &key, attempt)?;
            if entry.pipeline_id != Some(pipeline_id) {
                bail!("verification {key} attempt {attempt} does not own pipeline {pipeline_id}");
            }
            if entry.state != VerificationState::Testing {
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
            entry.attempt = entry
                .attempt
                .checked_add(1)
                .context("verification attempt counter overflowed")?;
            entry.state = VerificationState::Testing;
            entry.pipeline_id = None;
            entry.announced = false;
            entry.detail = None;
            entry.updated_at = unix_time()?;
            let retry = entry.clone();
            self.write(data)?;
            Ok(retry)
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
        let _lock = FileLock::exclusive(file)
            .with_context(|| format!("locking ledger {}", self.lock_path.display()))?;
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

fn require_attempt(entry: &LedgerEntry, key: &str, attempt: u64) -> Result<()> {
    if entry.attempt != attempt {
        bail!(
            "verification {key} is on attempt {}; refusing stale attempt {attempt}",
            entry.attempt
        );
    }
    Ok(())
}

fn testing_pipeline<'a>(
    data: &'a mut LedgerData,
    key: &str,
    attempt: u64,
    pipeline_id: u64,
) -> Result<&'a mut LedgerEntry> {
    let entry = testing_attempt(data, key, attempt)?;
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

    #[test]
    fn exact_head_and_base_reuse_one_durable_reservation() {
        let directory = tempfile::tempdir().unwrap();
        let ledger = Ledger::new(directory.path()).unwrap();

        let first = ledger.reserve("head", "base").unwrap();
        let second = ledger.reserve("head", "base").unwrap();

        assert_eq!(first, second);
        assert_eq!(first.attempt, 1);
        assert_eq!(first.pipeline_id, None);
    }

    #[test]
    fn changed_head_or_base_is_a_new_verification_key() {
        let directory = tempfile::tempdir().unwrap();
        let ledger = Ledger::new(directory.path()).unwrap();
        ledger.reserve("head-one", "base-one").unwrap();
        ledger.reserve("head-two", "base-one").unwrap();
        ledger.reserve("head-one", "base-two").unwrap();

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
    fn terminal_entries_and_pipeline_ids_are_immutable() {
        let directory = tempfile::tempdir().unwrap();
        let ledger = Ledger::new(directory.path()).unwrap();
        ledger.reserve("head", "base").unwrap();
        ledger.attach("head", "base", 1, 42).unwrap();
        ledger
            .finish(
                "head",
                "base",
                1,
                42,
                VerificationState::Verified,
                Some("passed".into()),
            )
            .unwrap();
        ledger
            .finish(
                "head",
                "base",
                1,
                42,
                VerificationState::Rejected,
                Some("late failure".into()),
            )
            .unwrap();

        let entry = ledger.get("head", "base").unwrap().unwrap();
        assert_eq!(entry.state, VerificationState::Verified);
        assert_eq!(entry.pipeline_id, Some(42));
        assert_eq!(entry.detail.as_deref(), Some("passed"));
        assert!(ledger.attach("head", "base", 1, 43).is_err());
    }

    #[test]
    fn retry_requires_the_exact_terminal_key() {
        let directory = tempfile::tempdir().unwrap();
        let ledger = Ledger::new(directory.path()).unwrap();
        ledger.reserve("head", "base").unwrap();
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

    #[test]
    fn policy_rejection_is_terminal_without_a_pipeline() {
        let directory = tempfile::tempdir().unwrap();
        let ledger = Ledger::new(directory.path()).unwrap();
        let entry = ledger.reserve("head", "base").unwrap();

        ledger
            .reject("head", "base", entry.attempt, "control path changed".into())
            .unwrap();

        let rejected = ledger.get("head", "base").unwrap().unwrap();
        assert_eq!(rejected.state, VerificationState::Rejected);
        assert_eq!(rejected.pipeline_id, None);
        assert_eq!(rejected.detail.as_deref(), Some("control path changed"));
    }
}

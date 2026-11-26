use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
};

use anyhow::{Context, Result, ensure};
use xtask::ci::lane::plan::wire;

use super::{
    api::view_path,
    command::PlanControl,
    ledger::Ledger,
    model::{CheckStatus, Scope},
    reconcile::VerificationAttempt,
};
use crate::ci::{
    environment::CacheTrust,
    lane::plan::{PlanRequest, PlanningPolicy, Purpose, VerificationIdentity, seal_view},
};

/// The status context that exports a verification's plan view. Every attempt
/// of one verification shares it, so each status follows the one before.
pub(super) fn context(attempt: &VerificationAttempt<'_>) -> String {
    format!("kithara/plan-control/{}", operation_key(attempt))
}

fn operation_key(attempt: &VerificationAttempt<'_>) -> String {
    format!("{}:{}", attempt.head_sha, attempt.base_sha)
}

/// Seal the plan view of an attempt about to be dispatched: the host's
/// protected policy resolves the public catalog for the judged merge the
/// bridge hands its run, with the draft test block it hands it too.
pub(super) fn seal(
    control: &PlanControl,
    attempt: &VerificationAttempt<'_>,
    pr_tests: Option<&str>,
    judged: &str,
    tree: &str,
    repository_id: u64,
    predecessor_status_id: Option<u64>,
) -> Result<Vec<u8>> {
    let policy: PlanningPolicy = serde_json::from_slice(
        &fs::read(&control.policy)
            .with_context(|| format!("reading planning policy {}", control.policy.display()))?,
    )
    .context("parsing the planning policy")?;
    let request = PlanRequest {
        identity: VerificationIdentity {
            repository: attempt.config.github_repo.clone(),
            head: attempt.head_sha.to_owned(),
            base: attempt.base_sha.to_owned(),
            policy_revision: policy.revision.clone(),
            expected_trees: BTreeMap::from([(Purpose::Merge, tree.to_owned())]),
        },
        kind: attempt.scope.kind(),
        full: false,
        only: BTreeSet::new(),
        changed_paths: None,
        narrow: None,
        draft_tests: pr_tests.map(str::to_owned),
        parameters: BTreeMap::new(),
    };
    let operation = operation(attempt, judged, tree, repository_id, predecessor_status_id);
    seal_view(
        &control.checkout,
        &policy,
        request,
        &operation,
        &control.consumer_source_pin,
    )
}

/// The attempt as its view names it: dispatched through the API onto a
/// quarantine branch, whose run executes the judged merge in `GitLab`.
fn operation(
    attempt: &VerificationAttempt<'_>,
    judged: &str,
    tree: &str,
    repository_id: u64,
    predecessor_status_id: Option<u64>,
) -> wire::Operation {
    let config = attempt.config;
    let repository = || wire::Repository {
        id: repository_id,
        name: config.github_repo.clone(),
    };
    let merge = wire::Execution {
        repository: wire::Repository {
            id: config.gitlab_project_id,
            name: config.gitlab_project_path.clone(),
        },
        commit: judged.to_owned(),
        tree: tree.to_owned(),
    };
    wire::Operation {
        key: operation_key(attempt),
        attempt: attempt.attempt,
        predecessor_status_id,
        original_kind: attempt.scope.kind().name().to_owned(),
        event_kind: "api".to_owned(),
        logical_trust: CacheTrust::Quarantine.as_str().to_owned(),
        scope: match attempt.scope {
            Scope::Full => "full",
            Scope::Draft => "draft",
        }
        .to_owned(),
        head_repository: repository(),
        status_repository: repository(),
        execution: BTreeMap::from([("merge".to_owned(), merge)]),
    }
}

/// Export an attempt's plan view before its run is dispatched.
///
/// A fresh attempt resolves its view once and freezes the bytes in the ledger
/// before anything leaves the host. Every later step works from those frozen
/// bytes: retention and readback, then the attestation, then the status that
/// exports it, last. A crash anywhere recovers the same bytes, never a second
/// resolution from a policy or test block that moved since. A judged merge
/// that moved fails closed, so nothing starts.
pub(super) fn publish(
    attempt: &VerificationAttempt<'_>,
    ledger: &Ledger,
    judged: &str,
    seal: impl FnOnce() -> Result<Vec<u8>>,
    retain: impl FnOnce(&[u8], &str) -> Result<String>,
    report: impl FnOnce(&CheckStatus) -> Result<()>,
) -> Result<()> {
    let (head, base, number) = (attempt.head_sha, attempt.base_sha, attempt.attempt);
    let frozen = ledger
        .get(head, base)?
        .filter(|entry| entry.attempt == number)
        .and_then(|entry| entry.plan);
    let seal = if let Some(seal) = frozen {
        ensure!(
            seal.judged == judged,
            "the judged merge moved under frozen plan view {}",
            seal.sha256
        );
        seal
    } else {
        ledger.freeze_plan(head, base, number, judged, &seal()?)?
    };
    let commit = if let Some(commit) = seal.commit {
        commit
    } else {
        let commit = retain(&ledger.frozen_view(&seal.sha256)?, &seal.sha256)?;
        ledger.attest_plan(head, base, number, &seal.sha256, &commit)?;
        commit
    };
    report(&CheckStatus {
        context: context(attempt),
        state: "success",
        description: format!("pc3:{}", seal.sha256),
        target_url: Some(format!(
            "https://github.com/{}/blob/{commit}/{}",
            attempt.config.github_repo,
            view_path(&seal.sha256)
        )),
    })
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use anyhow::bail;
    use serde_json::json;
    use sha2::{Digest as _, Sha256};

    use super::{super::command::BridgeConfig, *};

    const HEAD: &str = "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1";
    const BASE: &str = "b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2";

    fn config() -> BridgeConfig {
        toml::from_str(include_str!(
            "../../../../.config/bridge/config.example.toml"
        ))
        .unwrap()
    }

    fn attempt(config: &BridgeConfig) -> VerificationAttempt<'_> {
        VerificationAttempt {
            head_sha: HEAD,
            base_sha: BASE,
            attempt: 1,
            scope: Scope::Draft,
            config,
        }
    }

    fn ledger(directory: &tempfile::TempDir) -> Ledger {
        let ledger = Ledger::new(directory.path()).unwrap();
        ledger.reserve(HEAD, BASE, Scope::Draft).unwrap();
        ledger
    }

    fn digest(bytes: &[u8]) -> String {
        hex::encode(Sha256::digest(bytes))
    }

    #[test]
    fn a_fresh_attempt_freezes_retains_attests_and_exports_last() {
        let config = config();
        let attempt = attempt(&config);
        let directory = tempfile::tempdir().unwrap();
        let ledger = ledger(&directory);
        let judged = "c".repeat(40);
        let events = RefCell::new(Vec::new());
        let plan = || ledger.get(HEAD, BASE).unwrap().unwrap().plan;
        publish(
            &attempt,
            &ledger,
            &judged,
            || {
                events.borrow_mut().push("seal");
                Ok(b"{}".to_vec())
            },
            |bytes, sha256| {
                assert_eq!((bytes, sha256), (&b"{}"[..], digest(b"{}").as_str()));
                assert_eq!(plan().unwrap().commit, None, "frozen before it leaves");
                events.borrow_mut().push("retain");
                Ok("d".repeat(40))
            },
            |status| {
                assert_eq!(plan().unwrap().commit, Some("d".repeat(40)));
                assert_eq!(
                    status.context,
                    format!("kithara/plan-control/{HEAD}:{BASE}")
                );
                assert_eq!(status.state, "success");
                assert_eq!(status.description, format!("pc3:{}", digest(b"{}")));
                assert_eq!(
                    status.target_url,
                    Some(format!(
                        "https://github.com/{}/blob/{}/.ci-artifacts/plan-control/v3/{}.json",
                        config.github_repo,
                        "d".repeat(40),
                        digest(b"{}")
                    ))
                );
                events.borrow_mut().push("report");
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(*events.borrow(), ["seal", "retain", "report"]);
    }

    /// The crash the freeze exists for: retention went through, the
    /// attestation never landed, and the policy and the draft's test block
    /// moved before the next tick. That tick must carry the bytes it froze,
    /// not resolve a second plan for the same attempt, and must not export
    /// or dispatch anything until those bytes are attested.
    #[test]
    fn a_crash_after_retention_recovers_the_frozen_bytes_never_a_new_plan() {
        let config = config();
        let attempt = attempt(&config);
        let directory = tempfile::tempdir().unwrap();
        let ledger = ledger(&directory);
        let judged = "c".repeat(40);
        let original = br#"{"policy":"old"}"#;
        let crashed = publish(
            &attempt,
            &ledger,
            &judged,
            || Ok(original.to_vec()),
            |_, _| bail!("lost after the retention branch moved"),
            |_| panic!("a view was exported before its attestation"),
        );
        assert!(crashed.is_err(), "a failed tick must not reach dispatch");

        let retained = RefCell::new(None);
        let exported = RefCell::new(None);
        publish(
            &attempt,
            &Ledger::new(directory.path()).unwrap(),
            &judged,
            || panic!("the same attempt resolved a second plan"),
            |bytes, sha256| {
                retained.replace(Some((bytes.to_vec(), sha256.to_owned())));
                Ok("d".repeat(40))
            },
            |status| {
                exported.replace(Some(status.description.clone()));
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(
            retained.into_inner(),
            Some((original.to_vec(), digest(original)))
        );
        assert_eq!(
            exported.into_inner(),
            Some(format!("pc3:{}", digest(original)))
        );

        publish(
            &attempt,
            &ledger,
            &judged,
            || panic!("an attested attempt resolved again"),
            |_, _| panic!("an attested attempt retained again"),
            |status| {
                assert_eq!(status.description, format!("pc3:{}", digest(original)));
                Ok(())
            },
        )
        .unwrap();
    }

    #[test]
    fn a_judged_merge_that_moved_under_a_frozen_view_fails_closed() {
        let config = config();
        let attempt = attempt(&config);
        let directory = tempfile::tempdir().unwrap();
        let ledger = ledger(&directory);
        ledger
            .freeze_plan(HEAD, BASE, 1, &"c".repeat(40), b"{}")
            .unwrap();
        let moved = publish(
            &attempt,
            &ledger,
            &"f".repeat(40),
            || panic!("a moved merge resolved again"),
            |_, _| panic!("a moved merge retained its stale view"),
            |_| panic!("a moved merge exported a stale view"),
        );
        assert!(moved.is_err());
    }

    #[test]
    fn the_operation_names_the_attempt_and_the_merge_its_run_executes() {
        let config = config();
        let operation = operation(
            &attempt(&config),
            &"c".repeat(40),
            &"d".repeat(40),
            7,
            Some(9),
        );
        let repository = json!({"id": 7, "name": config.github_repo});
        assert_eq!(
            serde_json::to_value(&operation).unwrap(),
            json!({
                "key": format!("{HEAD}:{BASE}"),
                "attempt": 1,
                "predecessor_status_id": 9,
                "original_kind": "branch",
                "event_kind": "api",
                "logical_trust": "quarantine",
                "scope": "draft",
                "head_repository": repository,
                "status_repository": repository,
                "execution": {"merge": {
                    "repository": {"id": config.gitlab_project_id, "name": config.gitlab_project_path},
                    "commit": "c".repeat(40),
                    "tree": "d".repeat(40),
                }},
            })
        );
    }
}

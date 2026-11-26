use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
};

use anyhow::{Context, Result, ensure};
use sha2::{Digest as _, Sha256};
use xtask::ci::lane::plan::wire;

use super::{
    api::view_path,
    command::PlanControl,
    ledger::PlanSeal,
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
/// A fresh attempt seals its view, retains the bytes, records the seal in the
/// ledger and only then publishes the status that exports it. An attempt that
/// sealed one already publishes that same record, unless the merge it judged
/// has moved since: the view no longer describes the run, so nothing starts.
pub(super) fn publish(
    attempt: &VerificationAttempt<'_>,
    sealed: Option<&PlanSeal>,
    judged: &str,
    seal: impl FnOnce() -> Result<Vec<u8>>,
    retain: impl FnOnce(&[u8], &str) -> Result<String>,
    record: impl FnOnce(&PlanSeal) -> Result<()>,
    report: impl FnOnce(&CheckStatus) -> Result<()>,
) -> Result<()> {
    let seal = if let Some(sealed) = sealed {
        ensure!(
            sealed.judged == judged,
            "the judged merge moved under sealed plan view {}",
            sealed.sha256
        );
        sealed.clone()
    } else {
        let bytes = seal()?;
        let sha256 = hex::encode(Sha256::digest(&bytes));
        let commit = retain(&bytes, &sha256)?;
        let seal = PlanSeal {
            sha256,
            commit,
            judged: judged.to_owned(),
        };
        record(&seal)?;
        seal
    };
    report(&CheckStatus {
        context: context(attempt),
        state: "success",
        description: format!("pc3:{}", seal.sha256),
        target_url: Some(format!(
            "https://github.com/{}/blob/{}/{}",
            attempt.config.github_repo,
            seal.commit,
            view_path(&seal.sha256)
        )),
    })
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use anyhow::bail;
    use serde_json::json;

    use super::{super::command::BridgeConfig, *};

    fn config() -> BridgeConfig {
        toml::from_str(include_str!(
            "../../../../.config/bridge/config.example.toml"
        ))
        .unwrap()
    }

    fn attempt(config: &BridgeConfig) -> VerificationAttempt<'_> {
        VerificationAttempt {
            head_sha: "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1",
            base_sha: "b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2",
            attempt: 2,
            scope: Scope::Draft,
            config,
        }
    }

    #[test]
    fn a_fresh_attempt_exports_its_view_only_after_retention_and_the_ledger() {
        let config = config();
        let attempt = attempt(&config);
        let judged = "c".repeat(40);
        let events = RefCell::new(Vec::new());
        let sha256 = hex::encode(Sha256::digest(b"{}"));
        publish(
            &attempt,
            None,
            &judged,
            || {
                events.borrow_mut().push("seal");
                Ok(b"{}".to_vec())
            },
            |bytes, digest| {
                assert_eq!((bytes, digest), (&b"{}"[..], sha256.as_str()));
                events.borrow_mut().push("retain");
                Ok("d".repeat(40))
            },
            |seal| {
                assert_eq!(seal.judged, judged);
                events.borrow_mut().push("record");
                Ok(())
            },
            |status| {
                assert_eq!(
                    status.context,
                    format!(
                        "kithara/plan-control/{}:{}",
                        attempt.head_sha, attempt.base_sha
                    )
                );
                assert_eq!(
                    (status.state, status.description.as_str()),
                    ("success", format!("pc3:{sha256}").as_str())
                );
                assert_eq!(
                    status.target_url.as_deref(),
                    Some(
                        format!(
                            "https://github.com/{}/blob/{}/.ci-artifacts/plan-control/v3/{sha256}.json",
                            config.github_repo,
                            "d".repeat(40)
                        )
                        .as_str()
                    )
                );
                events.borrow_mut().push("report");
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(*events.borrow(), ["seal", "retain", "record", "report"]);
    }

    #[test]
    fn bytes_that_were_not_retained_are_never_recorded_or_exported() {
        let config = config();
        let result = publish(
            &attempt(&config),
            None,
            &"c".repeat(40),
            || Ok(b"{}".to_vec()),
            |_, _| bail!("retention branch moved"),
            |_| panic!("an unretained view was recorded"),
            |_| panic!("an unretained view was exported"),
        );
        assert!(result.is_err());
    }

    #[test]
    fn a_sealed_attempt_exports_its_record_again_and_never_reseals() {
        let config = config();
        let attempt = attempt(&config);
        let sealed = PlanSeal {
            sha256: "e".repeat(64),
            commit: "d".repeat(40),
            judged: "c".repeat(40),
        };
        let exported = RefCell::new(None);
        publish(
            &attempt,
            Some(&sealed),
            &sealed.judged,
            || panic!("a sealed attempt resolved again"),
            |_, _| panic!("a sealed attempt retained again"),
            |_| panic!("a sealed attempt recorded again"),
            |status| {
                exported.replace(Some(status.description.clone()));
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(
            exported.into_inner(),
            Some(format!("pc3:{}", sealed.sha256))
        );

        let moved = publish(
            &attempt,
            Some(&sealed),
            &"f".repeat(40),
            || panic!("a moved merge resolved again"),
            |_, _| panic!("a moved merge retained again"),
            |_| panic!("a moved merge recorded again"),
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
                "key": "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1:b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2",
                "attempt": 2,
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

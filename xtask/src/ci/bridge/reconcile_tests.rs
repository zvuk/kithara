use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use super::*;
use crate::consts;

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
        head,
        3,
        &[],
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
        |sha, state, _| {
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
        "head",
        42,
        |id| {
            assert_eq!(id, 42);
            *calls.borrow_mut() += 1;
            Ok(PipelineObservation::Running)
        },
        |_, _, _| panic!("running is not a new commit-status transition"),
        |_, _, _| panic!("running must stay Testing"),
        |_| panic!("a running pipeline was not stopped"),
    )
    .unwrap();
    assert_eq!(*calls.borrow(), 1);
}

#[test]
fn success_is_posted_before_verified() {
    let actions = RefCell::new(Vec::new());
    observe_verification(
        "head",
        42,
        |id| {
            actions.borrow_mut().push(VerificationAction::Observe(id));
            Ok(PipelineObservation::Succeeded)
        },
        |sha, state, _| {
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
            "head",
            42,
            |_| Ok(observation.clone()),
            |_, state, _| {
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
        "head",
        42,
        |id| {
            actions.borrow_mut().push(VerificationAction::Observe(id));
            Ok(PipelineObservation::Cancelled)
        },
        |sha, state, _| {
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
        "head",
        42,
        |_| bail!("temporary GitLab API failure"),
        |_, _, _| panic!("an API error is not a verdict"),
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
    let error = recover_or_create(&[41, 42], || panic!("ambiguity must not create")).unwrap_err();
    assert!(error.to_string().contains("multiple pipelines"));
}

#[test]
fn an_attempt_ref_changes_only_when_retry_advances_the_generation() {
    assert_eq!(
        quarantine_ref("head", "base", 1),
        "quarantine/gh/head-base/attempt-1"
    );
    assert_eq!(
        quarantine_ref("head", "base", 2),
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
        quarantine_ref(head, base, 1),
        quarantine_ref(other_head, base, 1)
    );
    assert_ne!(
        quarantine_ref(head, base, 1),
        quarantine_ref(head, other_base, 1)
    );
}

#[test]
fn control_path_changes_post_failure_and_reject_without_a_pipeline() {
    let directory = tempfile::tempdir().unwrap();
    let ledger = Ledger::new(directory.path()).unwrap();
    let entry = ledger.reserve("head", "base").unwrap();
    let actions = RefCell::new(Vec::new());

    let handled = reject_control_changes(
        427,
        "head",
        &entry,
        &[".gitlab-ci.yml".into()],
        |sha, state, _| {
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
    let entry = ledger.reserve("head", "base").unwrap();

    assert!(
        !reject_control_changes(
            427,
            "head",
            &entry,
            &[],
            |_, _, _| panic!("product-only changes must not report a policy failure"),
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
    let entry = ledger.reserve("head", "base").unwrap();
    let actions = RefCell::new(Vec::new());

    reject_unmergeable(
        118,
        "head",
        "base",
        &entry,
        |sha, state, detail| {
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

fn live_heads() -> Vec<String> {
    vec![consts::PULL_HEAD.to_owned()]
}

#[test]
fn a_quarantine_ref_judged_against_an_older_base_is_superseded() {
    let refs = [quarantine_ref(consts::PULL_HEAD, consts::OLDER_BASE, 1)];

    assert_eq!(
        superseded_quarantine_refs(&refs, consts::CURRENT_BASE, &live_heads()),
        [refs[0].as_str()]
    );
}

#[test]
fn a_quarantine_ref_judged_against_the_current_base_is_kept() {
    let refs = [quarantine_ref(consts::PULL_HEAD, consts::CURRENT_BASE, 2)];

    assert_eq!(
        superseded_quarantine_refs(&refs, consts::CURRENT_BASE, &live_heads()),
        [] as [&str; 0]
    );
}

#[test]
fn a_branch_that_is_not_a_verification_run_is_never_swept() {
    let refs = ["develop".to_owned(), "laba/419-connectivity".to_owned()];

    assert_eq!(
        superseded_quarantine_refs(&refs, consts::CURRENT_BASE, &live_heads()),
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
        superseded_quarantine_refs(&refs, consts::CURRENT_BASE, &live_heads()),
        [refs[0].as_str()]
    );
}

#[test]
fn a_sweep_drops_every_branch_a_moved_base_left_behind() {
    let stale = quarantine_ref(consts::PULL_HEAD, consts::OLDER_BASE, 1);
    let live = quarantine_ref(consts::PULL_HEAD, consts::CURRENT_BASE, 2);
    let listed = vec![stale.clone(), live, "develop".to_owned()];
    let deleted = RefCell::new(Vec::new());

    sweep_quarantine_refs(
        consts::CURRENT_BASE,
        &live_heads(),
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
        consts::CURRENT_BASE,
        &live_heads(),
        || {
            Ok(vec![quarantine_ref(
                consts::PULL_HEAD,
                consts::CURRENT_BASE,
                1,
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
    )];

    assert_eq!(
        superseded_quarantine_refs(&refs, consts::CURRENT_BASE, &live_heads()),
        [refs[0].as_str()]
    );
}

#[test]
fn a_quarantine_ref_for_an_open_pull_requests_head_is_kept() {
    let refs = [quarantine_ref(consts::PULL_HEAD, consts::CURRENT_BASE, 1)];

    assert_eq!(
        superseded_quarantine_refs(&refs, consts::CURRENT_BASE, &live_heads()),
        [] as [&str; 0]
    );
}

/// Deleting the branch does not stop its run: `GitLab` keeps a queued pipeline
/// in the resource group after the ref it names is gone, so the slot stays
/// taken by a commit nobody will merge.
#[test]
fn a_sweep_cancels_the_run_of_every_branch_it_drops() {
    let stale = quarantine_ref(consts::RETIRED_HEAD, consts::CURRENT_BASE, 1);
    let live = quarantine_ref(consts::PULL_HEAD, consts::CURRENT_BASE, 1);
    let listed = vec![stale.clone(), live, "develop".to_owned()];
    let cancelled = RefCell::new(Vec::new());

    sweep_quarantine_refs(
        consts::CURRENT_BASE,
        &live_heads(),
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

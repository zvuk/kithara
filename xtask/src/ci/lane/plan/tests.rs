use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};

use super::{
    catalog::Catalog,
    request::{
        PlacementMode, PlanRequest, PlanningPolicy, Platform, Pool, Purpose, VerificationIdentity,
    },
    resolve::resolve,
};
use crate::{
    ci::{lane::select::Fleet, run::PipelineKind},
    config::KitharaExt,
    testing::workspace::root,
};

fn plan_request(only: &[&str]) -> PlanRequest {
    PlanRequest {
        identity: VerificationIdentity {
            repository: "source/project".to_owned(),
            head: "a".repeat(40),
            base: "b".repeat(40),
            policy_revision: "c".repeat(40),
            expected_trees: BTreeMap::from([(Purpose::SourceHead, "d".repeat(40))]),
        },
        kind: PipelineKind::MergeRequest,
        full: false,
        only: only.iter().map(|id| (*id).to_owned()).collect(),
        changed_paths: None,
        narrow: None,
        draft_tests: None,
        parameters: BTreeMap::new(),
    }
}

fn policy(catalog: &Catalog, request: &PlanRequest) -> PlanningPolicy {
    let platforms = BTreeSet::from([
        Platform {
            os: "linux".to_owned(),
            arch: "x86_64".to_owned(),
        },
        Platform {
            os: "macos".to_owned(),
            arch: "aarch64".to_owned(),
        },
        Platform {
            os: "windows".to_owned(),
            arch: "aarch64".to_owned(),
        },
        Platform {
            os: "windows".to_owned(),
            arch: "x86_64".to_owned(),
        },
    ]);
    let capabilities = catalog
        .checks
        .values()
        .flat_map(|check| check.capabilities.iter().cloned())
        .collect::<BTreeSet<_>>();
    let mut pools = BTreeMap::new();
    let mut assignments = BTreeMap::new();
    for platform in &platforms {
        let name = format!("pool-{}-{}", platform.os, platform.arch.replace('_', "-"));
        pools.insert(
            name.clone(),
            Pool {
                provider: if platform.os == "linux" {
                    Fleet::Github
                } else {
                    Fleet::Gitlab
                },
                platform: platform.clone(),
                enabled: true,
                capabilities: capabilities.clone(),
            },
        );
        for id in catalog.checks.keys() {
            for purpose in request.identity.expected_trees.keys() {
                let purpose = match purpose {
                    Purpose::SourceHead => "source-head",
                    Purpose::Merge => "merge",
                    Purpose::Revision => "revision",
                };
                assignments.insert(
                    format!("{id}@{purpose}/{}/{}", platform.os, platform.arch),
                    vec![name.clone()],
                );
            }
        }
    }
    PlanningPolicy {
        revision: request.identity.policy_revision.clone(),
        catalog_digest: catalog.fingerprint().unwrap(),
        mode: PlacementMode::Hybrid,
        platforms,
        pools,
        assignments,
        qualification: BTreeSet::new(),
        inherited_timeouts: catalog
            .checks
            .iter()
            .filter(|(_, check)| check.timeout_minutes.is_none())
            .map(|(id, _)| (id.clone(), 60))
            .collect(),
    }
}

fn value(
    catalog: &Catalog,
    ext: &KitharaExt,
    request: PlanRequest,
    policy: &PlanningPolicy,
) -> Value {
    serde_json::to_value(resolve(catalog, &ext.ci, request, policy).unwrap()).unwrap()
}

#[test]
fn inventory_classifies_every_native_entrypoint_and_existing_lane_owner() {
    let ext = KitharaExt::load(&root()).unwrap();
    let catalog = Catalog::load(&root(), &ext.ci).unwrap();
    assert_eq!(ext.ci.lanes.len(), 67);
    assert_eq!(ext.ci.planning.residuals.len(), 8);
    assert_eq!(catalog.bindings.len(), 82);
    assert_eq!(catalog.checks.len(), 90);
    for id in [
        "network",
        "stress",
        "test-census",
        "windows-diagnostics",
        "host-provision",
        "release-publish",
        "verdict",
    ] {
        assert!(catalog.checks.contains_key(id));
    }
}

#[test]
fn unclassified_shadowed_and_stale_inventory_entries_fail_closed() {
    for fault in 0..3 {
        let mut ext = KitharaExt::load(&root()).unwrap();
        match fault {
            0 => ext.ci.lanes.get_mut("linux-lint").unwrap().semantics = None,
            1 => {
                let lane = ext.ci.lanes["linux-lint"].clone();
                ext.ci.lanes.insert("verdict".to_owned(), lane);
            }
            _ => {
                ext.ci.planning.bindings.pop_last();
            }
        }
        assert!(Catalog::load(&root(), &ext.ci).is_err());
    }
}

#[test]
fn equivalent_review_events_resolve_one_normalized_contract() {
    let ext = KitharaExt::load(&root()).unwrap();
    let catalog = Catalog::load(&root(), &ext.ci).unwrap();
    let source = plan_request(&["linux-lint"]);
    let policy = policy(&catalog, &source);
    let a = value(&catalog, &ext, source.clone(), &policy);
    let mut merge = source;
    merge.kind = PipelineKind::Quarantine;
    assert_eq!(a, value(&catalog, &ext, merge, &policy));
}

#[test]
fn placement_and_pool_changes_do_not_rewrite_logical_contracts() {
    let ext = KitharaExt::load(&root()).unwrap();
    let catalog = Catalog::load(&root(), &ext.ci).unwrap();
    let request = plan_request(&["linux-lint"]);
    let mut policy = policy(&catalog, &request);
    let before = value(&catalog, &ext, request.clone(), &policy);
    policy.pools.get_mut("pool-linux-x86-64").unwrap().provider = Fleet::Gitlab;
    policy.mode = PlacementMode::GitlabOnly;
    let after = value(&catalog, &ext, request, &policy);
    assert_eq!(before["contract_digest"], after["contract_digest"]);
    assert_ne!(before["generation"], after["generation"]);
    assert_eq!(after["providers"]["github"], json!([]));
    assert_eq!(after["providers"]["gitlab"].as_array().unwrap().len(), 1);
}

#[test]
fn disabled_github_macos_does_not_queue_and_can_be_enabled_without_rewriting_definitions() {
    let ext = KitharaExt::load(&root()).unwrap();
    let catalog = Catalog::load(&root(), &ext.ci).unwrap();
    let request = plan_request(&["apple-test"]);
    let mut policy = policy(&catalog, &request);
    let mut github = policy.pools["pool-macos-aarch64"].clone();
    github.provider = Fleet::Github;
    github.enabled = false;
    policy.pools.insert("github-macos".to_owned(), github);
    let original = value(&catalog, &ext, request.clone(), &policy);
    assert_eq!(original["providers"]["github"], json!([]));
    let id = "apple-test@source-head/macos/aarch64";
    policy
        .assignments
        .insert(id.to_owned(), vec!["github-macos".to_owned()]);
    assert!(resolve(&catalog, &ext.ci, request.clone(), &policy).is_err());
    policy.pools.get_mut("github-macos").unwrap().enabled = true;
    let enabled = value(&catalog, &ext, request, &policy);
    assert_eq!(enabled["providers"]["github"].as_array().unwrap().len(), 1);
    assert_eq!(original["contract_digest"], enabled["contract_digest"]);
}

#[test]
fn absent_conflicting_and_ineligible_owners_cannot_emit_successful_empty_plans() {
    let ext = KitharaExt::load(&root()).unwrap();
    let catalog = Catalog::load(&root(), &ext.ci).unwrap();
    let request = plan_request(&["linux-lint"]);
    let original = policy(&catalog, &request);
    let id = "linux-lint@source-head/linux/x86_64";
    for fault in 0..5 {
        let mut policy = original.clone();
        match fault {
            0 => {
                policy.assignments.remove(id);
            }
            1 => {
                policy
                    .assignments
                    .insert(id.to_owned(), vec!["pool-linux-x86-64".to_owned(); 2]);
            }
            2 => policy.pools.get_mut("pool-linux-x86-64").unwrap().enabled = false,
            3 => {
                policy
                    .pools
                    .get_mut("pool-linux-x86-64")
                    .unwrap()
                    .platform
                    .arch = "aarch64".to_owned();
            }
            _ => policy.mode = PlacementMode::GitlabOnly,
        }
        assert!(resolve(&catalog, &ext.ci, request.clone(), &policy).is_err());
    }
    let mut policy = original;
    policy
        .pools
        .get_mut("pool-linux-x86-64")
        .unwrap()
        .capabilities
        .clear();
    let request = plan_request(&["web-chromium"]);
    assert!(resolve(&catalog, &ext.ci, request, &policy).is_err());
}

#[test]
fn only_explicit_bounded_qualification_can_admit_two_providers() {
    let ext = KitharaExt::load(&root()).unwrap();
    let catalog = Catalog::load(&root(), &ext.ci).unwrap();
    let request = plan_request(&["linux-lint"]);
    let mut policy = policy(&catalog, &request);
    let id = "linux-lint@source-head/linux/x86_64";
    let mut pool = policy.pools["pool-linux-x86-64"].clone();
    pool.provider = Fleet::Gitlab;
    policy.pools.insert("gitlab-linux".to_owned(), pool);
    policy
        .assignments
        .get_mut(id)
        .unwrap()
        .push("gitlab-linux".to_owned());
    assert!(resolve(&catalog, &ext.ci, request.clone(), &policy).is_err());
    policy.mode = PlacementMode::DualQualification;
    policy.qualification.insert(id.to_owned());
    let plan = value(&catalog, &ext, request, &policy);
    assert_eq!(plan["providers"]["github"].as_array().unwrap().len(), 1);
    assert_eq!(plan["providers"]["gitlab"].as_array().unwrap().len(), 1);
}

#[test]
fn identity_cannot_substitute_another_tree_scope_filter_policy_or_report() {
    let ext = KitharaExt::load(&root()).unwrap();
    let mut catalog = Catalog::load(&root(), &ext.ci).unwrap();
    let original = plan_request(&["linux-net-host"]);
    let policy = policy(&catalog, &original);
    let baseline = value(&catalog, &ext, original.clone(), &policy);
    for axis in 0..4 {
        let mut request = original.clone();
        match axis {
            0 => {
                request
                    .identity
                    .expected_trees
                    .insert(Purpose::SourceHead, "e".repeat(40));
            }
            1 => request.narrow = Some("package(xtask)".to_owned()),
            2 => request.identity.head = "e".repeat(40),
            _ => request.identity.base = "e".repeat(40),
        }
        assert_ne!(
            baseline["contract_digest"],
            value(&catalog, &ext, request, &policy)["contract_digest"]
        );
    }
    let mut request = original.clone();
    request.identity.policy_revision = "e".repeat(40);
    assert!(resolve(&catalog, &ext.ci, request, &policy).is_err());
    catalog.checks.get_mut("linux-net-host").unwrap().contract["semantics"]["acceptance"] =
        json!("different-report-policy");
    assert!(resolve(&catalog, &ext.ci, original.clone(), &policy).is_err());
    let mut reviewed = policy;
    reviewed.catalog_digest = catalog.fingerprint().unwrap();
    assert_ne!(
        baseline["contract_digest"],
        value(&catalog, &ext, original, &reviewed)["contract_digest"]
    );
}

#[test]
fn source_and_merge_purposes_keep_distinct_contracts_even_for_the_same_tree() {
    let ext = KitharaExt::load(&root()).unwrap();
    let catalog = Catalog::load(&root(), &ext.ci).unwrap();
    let mut request = plan_request(&["linux-lint"]);
    request
        .identity
        .expected_trees
        .insert(Purpose::Merge, "d".repeat(40));
    let policy = policy(&catalog, &request);
    let plan = value(&catalog, &ext, request, &policy);
    let checks = plan["checks"].as_object().unwrap();
    assert_eq!(checks.len(), 2);
    let mut digests = checks
        .values()
        .map(|check| check["digest"].as_str().unwrap())
        .collect::<Vec<_>>();
    digests.sort();
    digests.dedup();
    assert_eq!(digests.len(), 2);
}

#[test]
fn manual_full_includes_dedicated_checks_but_never_publication_or_provisioning() {
    let ext = KitharaExt::load(&root()).unwrap();
    let catalog = Catalog::load(&root(), &ext.ci).unwrap();
    let mut request = plan_request(&[]);
    request.full = true;
    let mut policy = policy(&catalog, &request);
    let plan = value(&catalog, &ext, request.clone(), &policy);
    let names = plan["checks"]
        .as_object()
        .unwrap()
        .values()
        .map(|check| check["definition"].as_str().unwrap())
        .collect::<BTreeSet<_>>();
    for id in [
        "network",
        "stress",
        "test-census",
        "apple-ios-test",
        "release-docs",
    ] {
        assert!(names.contains(id));
    }
    for id in [
        "release-publish",
        "host-provision",
        "release-app",
        "windows-diagnostics",
    ] {
        assert!(!names.contains(id));
        assert!(plan["graph"]["definitions"].get(id).is_some());
    }
    policy.pools.get_mut("pool-macos-aarch64").unwrap().enabled = false;
    assert!(resolve(&catalog, &ext.ci, request, &policy).is_err());
}

#[test]
fn candidate_request_cannot_carry_placement_or_weaken_full_inventory() {
    let mut raw = serde_json::to_value(plan_request(&["linux-lint"])).unwrap();
    raw["assignments"] = json!({});
    assert!(serde_json::from_value::<PlanRequest>(raw).is_err());
    let mut request = plan_request(&["linux-lint"]);
    request.full = true;
    assert!(request.normalized().is_err());
}

#[test]
fn a_check_digest_binds_draft_versus_ready_harness_selection() {
    let ext = KitharaExt::load(&root()).unwrap();
    let catalog = Catalog::load(&root(), &ext.ci).unwrap();
    let ready = plan_request(&["linux-test-real-clock"]);
    let policy = policy(&catalog, &ready);
    let ready_plan = value(&catalog, &ext, ready.clone(), &policy);
    let mut draft = ready;
    draft.kind = PipelineKind::Branch;
    let draft_plan = value(&catalog, &ext, draft, &policy);
    let id = "linux-test-real-clock@source-head/linux/x86_64";
    assert_ne!(
        ready_plan["checks"][id]["digest"],
        draft_plan["checks"][id]["digest"]
    );
}

#[test]
fn initial_execution_contracts_match_the_unchanged_legacy_selector() {
    let ext = KitharaExt::load(&root()).unwrap();
    let expected: BTreeMap<String, String> = serde_json::from_str(include_str!(
        "../../../../tests/fixtures/ci-plan-legacy.json"
    ))
    .unwrap();
    assert_eq!(super::legacy::fingerprints(&ext.ci).unwrap(), expected);
}

#[test]
fn draft_scope_uses_the_existing_harness_union_and_manual_lanes_stay_manual() {
    let ext = KitharaExt::load(&root()).unwrap();
    let catalog = Catalog::load(&root(), &ext.ci).unwrap();
    let ready = plan_request(&[]);
    let policy = policy(&catalog, &ready);
    let plan = value(&catalog, &ext, ready.clone(), &policy);
    let names = plan["checks"]
        .as_object()
        .unwrap()
        .values()
        .map(|check| check["definition"].as_str().unwrap())
        .collect::<BTreeSet<_>>();
    assert!(names.contains("apple-net-units"));
    assert!(!names.contains("deep-rtsan"));
    assert!(!names.contains("deep-bench"));
    let mut draft = ready;
    draft.kind = PipelineKind::Branch;
    let plan = value(&catalog, &ext, draft, &policy);
    assert!(
        !plan["checks"]
            .as_object()
            .unwrap()
            .values()
            .any(|check| check["definition"] == "apple-net-units")
    );
    let request = plan_request(&["deep-rtsan"]);
    assert!(resolve(&catalog, &ext.ci, request, &policy).is_ok());
}

#[test]
fn manual_full_rejects_disconnected_dependency_cycles() {
    let mut ext = KitharaExt::load(&root()).unwrap();
    let mut request = plan_request(&[]);
    request.full = true;
    let catalog = Catalog::load(&root(), &ext.ci).unwrap();
    let baseline_policy = policy(&catalog, &request);
    assert!(resolve(&catalog, &ext.ci, request.clone(), &baseline_policy).is_ok());

    for (id, predecessor) in [("linux-lint", "linux-arch"), ("linux-arch", "linux-lint")] {
        ext.ci.lanes.get_mut(id).unwrap().needs = vec![predecessor.to_owned()];
    }
    let catalog = Catalog::load(&root(), &ext.ci).unwrap();
    for full in [true, false] {
        request.full = full;
        let policy = policy(&catalog, &request);
        assert!(resolve(&catalog, &ext.ci, request.clone(), &policy).is_err());
    }
}

#[test]
fn missing_external_timeout_and_dependency_cycles_fail_before_admission() {
    let ext = KitharaExt::load(&root()).unwrap();
    let mut catalog = Catalog::load(&root(), &ext.ci).unwrap();
    let mut request = plan_request(&[]);
    request.full = true;
    let mut policy = policy(&catalog, &request);
    policy.inherited_timeouts.remove("release-docs");
    assert!(resolve(&catalog, &ext.ci, request, &policy).is_err());
    catalog
        .checks
        .get_mut("linux-lint")
        .unwrap()
        .needs
        .insert("linux-arch".to_owned());
    catalog.checks.get_mut("linux-arch").unwrap().needs =
        BTreeSet::from(["linux-lint".to_owned(), "linux-tooling".to_owned()]);
    policy.catalog_digest = catalog.fingerprint().unwrap();
    let request = plan_request(&["linux-lint", "linux-arch", "linux-tooling"]);
    assert!(resolve(&catalog, &ext.ci, request, &policy).is_err());
}

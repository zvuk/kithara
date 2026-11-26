use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    process::Command,
};

use anyhow::{Context, Result, ensure};
use kithara_devtools::Ctx;
use serde_json::{Value, json};
use sha2::Digest as _;

use super::{
    catalog::{Catalog, digest},
    request::{PlanRequest, PlanningPolicy, Platform},
    resolve::{Check, CheckMetadata, resolve, source_contract},
    wire,
};
use crate::config::{CiProjectConfig, KitharaExt};

/// Require `worktree` to be the clean checkout of the operator-bound policy revision.
pub(super) fn require_policy_checkout(worktree: &Path, revision: &str) -> Result<()> {
    let git = |args: &[&str]| -> Result<Vec<u8>> {
        let output = Command::new("git")
            .args(args)
            .current_dir(worktree)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .output()?;
        ensure!(output.status.success(), "policy checkout inspection failed");
        Ok(output.stdout)
    };
    ensure!(
        String::from_utf8(git(&["rev-parse", "HEAD"])?)?.trim() == revision,
        "policy checkout differs from operator-bound revision"
    );
    ensure!(
        git(&["status", "--porcelain", "--untracked-files=normal"])?.is_empty(),
        "policy checkout is dirty"
    );
    Ok(())
}

pub(super) fn prepare_view(
    ctx: &Ctx,
    request: PlanRequest,
    policy: &PlanningPolicy,
    operation: &wire::Operation,
    consumer_source_pin: &str,
) -> Result<wire::PlanViewV3> {
    ensure!(
        operation.original_kind == request.kind.name(),
        "original dispatch kind differs"
    );
    let config = KitharaExt::from_ctx(ctx)?;
    config.ci.validate()?;
    let catalog = Catalog::load(&ctx.root, &config.ci)?;
    let plan = resolve(&catalog, &config.ci, request, policy)?;
    let mut checks = BTreeMap::new();
    for (id, check) in &plan.checks {
        let metadata = &check.metadata;
        checks.insert(id.clone(), json!({"id":id,"definition":metadata.definition,
            "purpose":metadata.purpose,"platform":metadata.platform,"expected_tree":metadata.expected_tree,
            "timeout_minutes":metadata.timeout_minutes,"needs":metadata.needs,
            "source_contract_sha256":check.digest,"build_test_sha256":wire::digest(&catalog.build_and_test)?,
            "pins_sha256":wire::digest(&catalog.pins)?}));
    }
    let mut counts = BTreeMap::<&str, usize>::new();
    for admission in plan.providers.values().flatten() {
        *counts.entry(&admission.check).or_default() += 1;
    }
    let view: wire::PlanViewV3 = serde_json::from_value(
        json!({"version":3,"schema_digest":wire::SCHEMA_DIGEST,
        "consumer_source_pin":consumer_source_pin,"operation":operation,"request":plan.request,
        "attestation":{"catalog_digest":plan.catalog_digest,"contract_digest":plan.contract_digest,
            "generation":plan.generation,"raw_plan_sha256":wire::digest(&plan)?,
            "dual_checks":counts.iter().filter(|(_, count)| **count==2).map(|(id,_)| *id).collect::<Vec<_>>()},
        "checks":checks,"graph":graph(&catalog)?,"harness_runners":harness(&catalog)?,"providers":plan.providers}),
    )?;
    let pins = wire::ReadPins {
        schema_digest: wire::SCHEMA_DIGEST,
        consumer_source_pin,
    };
    let bytes = wire::canonical_json(&view)?;
    let view = wire::decode(&bytes, &hex::encode(sha2::Sha256::digest(&bytes)), &pins)?;
    verify_catalog(&view, &catalog, &config.ci)?;
    Ok(view)
}

fn graph(catalog: &Catalog) -> Result<Value> {
    let mut definitions = BTreeMap::new();
    for (name, definition) in &catalog.checks {
        let contract = &definition.contract;
        let shared = contract.get("family").is_none();
        let artifact = if shared && !contract["artifact"].is_null() {
            let value = &contract["artifact"];
            let paths: Vec<_> = value["path"]
                .as_str()
                .unwrap_or("")
                .lines()
                .map(|path| path.trim_end_matches('/'))
                .collect();
            Some(
                json!({"name":value["name"],"paths":paths,"report":value["report"],"when":value["when"]}),
            )
        } else {
            None
        };
        definitions.insert(name.clone(),json!({"kind":definition.kind,"platforms":definition.platforms,
            "kinds":definition.kinds,"capabilities":definition.capabilities,"needs":definition.needs,
            "parameters":definition.parameters,"timeout_minutes":definition.timeout_minutes,"path_globs":definition.path_globs,
            "recipe":{"family":if shared {json!("shared-lane")} else {contract["family"].clone()},
                "role":if shared {contract["role"].clone()} else {Value::Null},
                "owner":if shared {json!("xtask/src/ci/lane/declared.rs")} else {contract["owner"].clone()},
                "sources":if shared {json!({})} else {contract["sources"].clone()},
                "source_contract_sha256":wire::digest(contract)?,"artifact":artifact,"semantics":contract["semantics"]}}));
    }
    let roles: BTreeMap<_, _> = wire::ROLES
        .iter()
        .map(|role| {
            (
                *role,
                definitions
                    .iter()
                    .filter(|(_, definition)| definition["recipe"]["role"] == *role)
                    .map(|(name, _)| name.clone())
                    .collect::<Vec<_>>(),
            )
        })
        .collect();
    Ok(
        json!({"definitions":definitions,"entrypoints":catalog.bindings,"roles":roles,
        "non_validation":"comparison only; separate execution authority"}),
    )
}

fn harness(catalog: &Catalog) -> Result<Value> {
    let values = catalog.build_and_test["test"]["lanes"]
        .as_object()
        .context("test lane source")?
        .iter()
        .filter(|(_, lane)| !lane["runner"].is_null())
        .map(|(name, lane)| (name.clone(), lane["runner"].clone()))
        .collect::<BTreeMap<_, _>>();
    Ok(serde_json::to_value(values)?)
}

pub(super) fn verify_catalog(
    view: &wire::PlanViewV3,
    catalog: &Catalog,
    config: &CiProjectConfig,
) -> Result<()> {
    ensure!(
        view.attestation.catalog_digest == catalog.fingerprint()?,
        "public catalog differs"
    );
    ensure!(
        serde_json::to_value(&view.graph)? == graph(catalog)?,
        "incomplete or changed public graph"
    );
    ensure!(
        serde_json::to_value(&view.harness_runners)? == harness(catalog)?,
        "changed logical harness choices"
    );
    let request: PlanRequest = serde_json::from_value(serde_json::to_value(&view.request)?)?;
    let before = serde_json::to_value(&request)?;
    let request = request.normalized()?;
    ensure!(
        serde_json::to_value(&request)? == before,
        "non-normalized public request"
    );
    let mut checks = BTreeMap::new();
    for (id, check) in &view.checks {
        let metadata = CheckMetadata {
            id: check.id.clone(),
            definition: check.definition.clone(),
            purpose: serde_json::from_value(json!(check.purpose))?,
            platform: Platform {
                os: check.platform.os.clone(),
                arch: check.platform.arch.clone(),
            },
            expected_tree: check.expected_tree.clone(),
            timeout_minutes: check.timeout_minutes.try_into()?,
            needs: check.needs.iter().cloned().collect::<BTreeSet<_>>(),
        };
        ensure!(
            metadata.needs.iter().eq(check.needs.iter()),
            "non-canonical dependency set"
        );
        let source = source_contract(catalog, config, &request, &metadata)?;
        ensure!(
            digest(&serde_json::to_vec(&source)?) == check.source_contract_sha256
                && wire::digest(&catalog.build_and_test)? == check.build_test_sha256
                && wire::digest(&catalog.pins)? == check.pins_sha256,
            "public execution contract differs"
        );
        checks.insert(
            id,
            Check {
                metadata,
                contract: source,
                digest: check.source_contract_sha256.clone(),
            },
        );
    }
    ensure!(
        digest(&serde_json::to_vec(
            &json!({"request":request,"checks":checks})
        )?) == view.attestation.contract_digest,
        "partial or changed selected contract"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        super::tests::{plan_request, policy},
        *,
    };
    use crate::{ci::run::PipelineKind, testing::workspace::root};

    fn verify_sources(ctx: &Ctx, view: &wire::PlanViewV3) -> Result<()> {
        let config = KitharaExt::from_ctx(ctx)?;
        let catalog = Catalog::load(&ctx.root, &config.ci)?;
        verify_catalog(view, &catalog, &config.ci)
    }

    fn produced(trust: &str) -> (Ctx, wire::PlanViewV3) {
        let root = root();
        let ctx = Ctx::new(
            root.clone(),
            kithara_devtools::common::project::ProjectConfig::load(&root).unwrap(),
        );
        let config = KitharaExt::from_ctx(&ctx).unwrap();
        let catalog = Catalog::load(&root, &config.ci).unwrap();
        let mut request = plan_request(&["linux-lint", "linux-tooling"]);
        request.kind = PipelineKind::Branch;
        let mut placement = policy(&catalog, &request);
        placement
            .platforms
            .retain(|platform| platform.os == "linux");
        let operation: wire::Operation = serde_json::from_value(json!({"key":"test-operation","attempt":1,"predecessor_status_id":null,
            "original_kind":"branch","event_kind":"push","logical_trust":trust,"scope":"draft",
            "head_repository":{"id":2,"name":"source/project"},"status_repository":{"id":1,"name":"zvuk/kithara"},
            "execution":{"source-head":{"repository":{"id":2,"name":"source/project"},"commit":"a".repeat(40),"tree":"d".repeat(40)}}})).unwrap();
        let view = prepare_view(&ctx, request, &placement, &operation, &"e".repeat(40)).unwrap();
        (ctx, view)
    }

    #[test]
    fn positive_copy_preserves_complete_public_contracts_without_physical_bindings() {
        let (ctx, view) = produced("review");
        let config = KitharaExt::from_ctx(&ctx).unwrap();
        let catalog = Catalog::load(&ctx.root, &config.ci).unwrap();
        assert!(view.graph.definitions.keys().eq(catalog.checks.keys()));
        assert_eq!(
            view.graph.entrypoints,
            catalog
                .bindings
                .iter()
                .map(|(key, values)| (key.clone(), values.iter().cloned().collect()))
                .collect()
        );
        for (name, lane) in &config.ci.lanes {
            if let Some(artifact) = &lane.artifact {
                let exported = view.graph.definitions[name]
                    .recipe
                    .artifact
                    .as_ref()
                    .unwrap();
                let expected: Vec<_> = artifact
                    .path
                    .as_deref()
                    .unwrap_or("")
                    .lines()
                    .map(|path| path.trim_end_matches('/'))
                    .collect();
                assert_eq!(exported.paths, expected);
            }
        }
        assert!(
            view.graph
                .definitions
                .values()
                .any(|definition| definition.recipe.family == "ci-run")
        );
        assert!(
            view.graph
                .definitions
                .values()
                .any(|definition| definition.recipe.family == "dedicated")
        );
        let bytes = wire::canonical_json(&view).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        for excluded in [
            "\"cache_group\":",
            "\"env\":",
            "\"availability\":",
            "\"labels\":",
            "\"prepared\":",
        ] {
            assert!(!text.contains(excluded), "{excluded}");
        }
        verify_sources(&ctx, &view).unwrap();
    }

    #[test]
    fn source_verifier_rejects_partial_graph_and_changed_harness_without_policy_lookup() {
        let (ctx, mut view) = produced("review");
        view.harness_runners.clear();
        assert!(verify_sources(&ctx, &view).is_err());
        let (_, mut view) = produced("review");
        view.graph.definitions.remove("network");
        view.graph
            .entrypoints
            .retain(|_, targets| !targets.iter().any(|target| target == "network"));
        assert!(verify_sources(&ctx, &view).is_err());
    }

    #[test]
    fn logical_trust_changes_view_bytes_without_faking_a_generation_change() {
        let (_, review) = produced("review");
        let (_, quarantine) = produced("quarantine");
        assert_eq!(
            review.attestation.generation,
            quarantine.attestation.generation
        );
        assert_eq!(
            review.attestation.raw_plan_sha256,
            quarantine.attestation.raw_plan_sha256
        );
        assert_ne!(
            wire::digest(&review).unwrap(),
            wire::digest(&quarantine).unwrap()
        );
        assert_eq!(quarantine.operation.original_kind, "branch");
        assert_ne!(
            review.request.identity.policy_revision,
            review.request.identity.base
        );
    }
}

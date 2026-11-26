use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, ensure};
use serde::Serialize;
use serde_json::{Value, json};

use super::{
    catalog::{Catalog, commands, digest, normalize_kind},
    request::{PlacementMode, PlanRequest, PlanningPolicy, Platform, Purpose},
    wire,
};
use crate::{
    ci::lane::{declared, select::Fleet},
    config::CiProjectConfig,
};

#[derive(Serialize)]
pub(super) struct Check {
    #[serde(flatten)]
    pub(super) metadata: CheckMetadata,
    pub(super) contract: Value,
    pub(super) digest: String,
}

#[derive(Serialize)]
pub(super) struct CheckMetadata {
    pub(super) id: String,
    pub(super) definition: String,
    pub(super) purpose: Purpose,
    pub(super) platform: Platform,
    pub(super) expected_tree: String,
    pub(super) timeout_minutes: u32,
    pub(super) needs: BTreeSet<String>,
}

#[derive(Serialize)]
pub(super) struct Admission {
    pub(super) check: String,
    pub(super) pool: String,
}

#[derive(Serialize)]
pub(super) struct Plan {
    schema: u32,
    pub(super) request: PlanRequest,
    pub(super) catalog_digest: String,
    pub(super) contract_digest: String,
    pub(super) generation: String,
    pub(super) checks: BTreeMap<String, Check>,
    graph: Value,
    availability: Value,
    pub(super) providers: BTreeMap<Fleet, Vec<Admission>>,
}

pub(super) fn resolve(
    catalog: &Catalog,
    config: &CiProjectConfig,
    request: PlanRequest,
    policy: &PlanningPolicy,
) -> Result<Plan> {
    policy.validate()?;
    let request = request.normalized()?;
    let fingerprint = validate_inputs(catalog, &request, policy)?;
    validate_acyclic(catalog.checks.iter().map(|(id, check)| (id, &check.needs)))?;
    let selected = select_checks(catalog, config, &request)?;
    let checks = instantiate_checks(catalog, config, &request, policy, &selected)?;
    validate_acyclic(checks.iter().map(|(id, check)| (id, &check.metadata.needs)))?;
    ensure!(
        policy
            .qualification
            .iter()
            .all(|id| checks.contains_key(id)),
        "qualification names checks outside this selected plan"
    );
    let contract_digest = digest(&serde_json::to_vec(
        &json!({"request":request,"checks":checks}),
    )?);
    let providers = admit_checks(catalog, &checks, policy)?;
    let availability = serde_json::to_value(&policy.pools)?;
    let generation = digest(&serde_json::to_vec(
        &json!({"contract":contract_digest,"policy":policy}),
    )?);
    let graph = json!({"definitions":catalog.checks,"entrypoints":catalog.bindings,"non_validation":"comparison only; separate execution authority"});
    Ok(Plan {
        schema: 1,
        request,
        catalog_digest: fingerprint,
        contract_digest,
        generation,
        checks,
        graph,
        availability,
        providers,
    })
}

fn validate_inputs(
    catalog: &Catalog,
    request: &PlanRequest,
    policy: &PlanningPolicy,
) -> Result<String> {
    if let Some(body) = &request.draft_tests {
        kithara_devtools::test::validate_pr_tests(&catalog.root, body)?;
    }
    let fingerprint = catalog.fingerprint()?;
    ensure!(
        policy.catalog_digest == fingerprint,
        "candidate catalog differs from trusted policy; refusing weakened or drifted contracts"
    );
    ensure!(
        request.identity.policy_revision == policy.revision,
        "request cannot choose another trusted policy revision"
    );
    ensure!(
        policy.inherited_timeouts.keys().all(|id| catalog
            .checks
            .get(id)
            .is_some_and(|check| check.timeout_minutes.is_none())),
        "trusted inherited timeouts cannot override declared budgets or name unknown checks"
    );
    ensure!(
        request
            .only
            .iter()
            .all(|id| catalog.checks.contains_key(id)),
        "request names an unknown check"
    );
    for (id, parameters) in &request.parameters {
        let check = catalog
            .checks
            .get(id)
            .with_context(|| format!("parameters name unknown check {id}"))?;
        ensure!(
            parameters
                .keys()
                .all(|name| check.parameters.contains(name)),
            "{id} has unsupported command parameters"
        );
    }
    Ok(fingerprint)
}

fn select_checks(
    catalog: &Catalog,
    config: &CiProjectConfig,
    request: &PlanRequest,
) -> Result<BTreeSet<String>> {
    let mut selected = BTreeSet::new();
    for (id, check) in &catalog.checks {
        if !check.kind.validates() {
            continue;
        }
        if !check.needs.is_empty() {
            continue;
        }
        if !request.only.is_empty() {
            if !request.only.contains(id) {
                continue;
            }
        } else if !request.full && !check.kinds.contains(request.kind.name()) {
            continue;
        }
        if request.only.is_empty()
            && !request.full
            && !request.kind.is_full()
            && !check.path_globs.is_empty()
            && let Some(paths) = &request.changed_paths
        {
            let globs = check
                .path_globs
                .iter()
                .map(|value| glob::Pattern::new(value))
                .collect::<std::result::Result<Vec<_>, _>>()?;
            if !globs
                .iter()
                .any(|pattern| paths.iter().any(|path| pattern.matches(path)))
            {
                continue;
            }
        }
        if let Some(lane) = config.lanes.get(id)
            && !declared::runs_anything(
                lane,
                request.kind,
                request.narrow.as_deref(),
                &catalog.root,
                &catalog.project,
                request.draft_tests.as_deref(),
            )?
        {
            continue;
        }
        selected.insert(id.clone());
    }
    loop {
        let consumers: Vec<_> = catalog
            .checks
            .iter()
            .filter(|(id, check)| {
                check.kind.validates()
                    && !selected.contains(*id)
                    && !check.needs.is_empty()
                    && check.needs.iter().any(|need| selected.contains(need))
                    && (request.full
                        || request.only.contains(*id)
                        || (!request.only.is_empty() && config.lanes.contains_key(*id))
                        || check.kinds.contains(request.kind.name()))
            })
            .map(|(id, _)| id.clone())
            .collect();
        if consumers.is_empty() {
            break;
        }
        selected.extend(consumers);
    }
    ensure!(
        request.only.iter().all(|id| selected.contains(id)),
        "explicit check has no applicable validation or selected predecessor"
    );
    ensure!(
        !selected.is_empty(),
        "request selected no validation contracts"
    );
    for id in &selected {
        let check = &catalog.checks[id];
        ensure!(
            check.needs.is_empty() || check.needs.iter().any(|need| selected.contains(need)),
            "{id} has no selected predecessor"
        );
    }
    Ok(selected)
}

fn instantiate_checks(
    catalog: &Catalog,
    config: &CiProjectConfig,
    request: &PlanRequest,
    policy: &PlanningPolicy,
    selected: &BTreeSet<String>,
) -> Result<BTreeMap<String, Check>> {
    let mut checks = BTreeMap::new();
    for id in selected {
        let definition = &catalog.checks[id];
        let timeout_minutes = match definition.timeout_minutes {
            Some(value) => value,
            None => *policy.inherited_timeouts.get(id).with_context(|| {
                format!(
                    "{id} requires its existing inherited timeout to be frozen by trusted policy"
                )
            })?,
        };
        ensure!(timeout_minutes > 0, "{id} has no valid time budget");
        for platform in policy.platforms.iter().filter(|platform| {
            definition.platforms.contains(&platform.os)
                && definition
                    .capabilities
                    .iter()
                    .filter_map(|cap| cap.strip_prefix("arch:"))
                    .all(|arch| arch == platform.arch)
        }) {
            for (purpose, tree) in &request.identity.expected_trees {
                let key = check_id(id, *purpose, platform);
                let check = CheckMetadata {
                    id: key.clone(),
                    definition: id.clone(),
                    purpose: *purpose,
                    platform: platform.clone(),
                    expected_tree: tree.clone(),
                    timeout_minutes,
                    needs: BTreeSet::new(),
                };
                checks.insert(key, check);
            }
        }
        ensure!(
            checks.values().any(|check| check.definition == *id),
            "trusted platform axes cannot carry required check {id}"
        );
    }
    let dependencies: BTreeMap<_, _> = checks
        .iter()
        .map(|(id, check)| {
            let needs = checks
                .iter()
                .filter(|(_, producer)| {
                    catalog.checks[&check.definition]
                        .needs
                        .contains(&producer.definition)
                        && producer.purpose == check.purpose
                })
                .map(|(id, _)| id.clone())
                .collect();
            (id.clone(), needs)
        })
        .collect();
    let mut resolved = BTreeMap::new();
    for (id, needs) in dependencies {
        let check = checks.get_mut(&id).context("planned check")?;
        check.needs = needs;
    }
    for (id, metadata) in checks {
        let contract = source_contract(catalog, config, request, &metadata)?;
        let digest = digest(&serde_json::to_vec(&contract)?);
        resolved.insert(
            id,
            Check {
                metadata,
                contract,
                digest,
            },
        );
    }
    Ok(resolved)
}

pub(super) fn source_contract(
    catalog: &Catalog,
    config: &CiProjectConfig,
    request: &PlanRequest,
    check: &CheckMetadata,
) -> Result<Value> {
    let mut definition = catalog
        .checks
        .get(&check.definition)
        .context("source definition")?
        .contract
        .clone();
    if let Some(lane) = config.lanes.get(&check.definition) {
        definition["steps"] = commands(lane, normalize_kind(request.kind.name()));
    }
    Ok(
        json!({"definition":definition,"parameters":request.parameters.get(&check.definition),"verification":request.identity,"context":request.kind,"test_and_build":catalog.build_and_test,"pins":catalog.pins,"selection":{"narrow":request.narrow,"draft_tests":request.draft_tests},"job_retries":0,"timeout_minutes":check.timeout_minutes,"purpose":check.purpose,"platform":check.platform,"tree":check.expected_tree,"selected_dependencies":check.needs}),
    )
}

fn admit_checks(
    catalog: &Catalog,
    checks: &BTreeMap<String, Check>,
    policy: &PlanningPolicy,
) -> Result<BTreeMap<Fleet, Vec<Admission>>> {
    let mut providers = BTreeMap::from([(Fleet::Github, Vec::new()), (Fleet::Gitlab, Vec::new())]);
    for (id, check) in checks {
        let check = &check.metadata;
        let pools = policy
            .assignments
            .get(id)
            .with_context(|| format!("required check has no execution owner: {id}"))?;
        let dual =
            policy.mode == PlacementMode::DualQualification && policy.qualification.contains(id);
        ensure!(
            pools.len() == if dual { 2 } else { 1 },
            "{id} must have exactly one ordinary owner or two explicitly qualified owners"
        );
        let mut seen = BTreeSet::new();
        for name in pools {
            let pool = policy
                .pools
                .get(name)
                .with_context(|| format!("{id} references unknown pool {name}"))?;
            ensure!(
                seen.insert(pool.provider),
                "{id} has conflicting provider ownership"
            );
            ensure!(
                pool.enabled,
                "required check {id} is blocked: pool {name} disabled; no job admitted"
            );
            ensure!(
                pool.platform == check.platform,
                "{id} requires another platform/architecture"
            );
            ensure!(
                catalog.checks[&check.definition]
                    .capabilities
                    .is_subset(&pool.capabilities),
                "required check {id} is blocked: pool {name} lacks capabilities"
            );
            ensure!(
                policy.mode != PlacementMode::GitlabOnly || pool.provider == Fleet::Gitlab,
                "GitLab-only placement cannot require GitHub execution"
            );
            providers
                .get_mut(&pool.provider)
                .context("known provider")?
                .push(Admission {
                    check: id.clone(),
                    pool: name.clone(),
                });
        }
    }
    Ok(providers)
}

fn check_id(id: &str, purpose: Purpose, platform: &Platform) -> String {
    let purpose = match purpose {
        Purpose::SourceHead => "source-head",
        Purpose::Merge => "merge",
        Purpose::Revision => "revision",
    };
    format!("{id}@{purpose}/{}", platform.key())
}

fn validate_acyclic<'a>(
    dependencies: impl Iterator<Item = (&'a String, &'a BTreeSet<String>)>,
) -> Result<()> {
    wire::validate_acyclic(
        dependencies.map(|(id, needs)| (id.as_str(), needs.iter().map(String::as_str).collect())),
    )?;
    Ok(())
}

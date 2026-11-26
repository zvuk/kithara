use std::{collections::BTreeMap, fs};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{
    catalog::{Catalog, digest},
    request::Platform,
    view::verify_catalog,
    wire::{self, PlanViewV3},
};
use crate::{
    ci::{environment::CacheTrust, lane::select::Fleet, run::PipelineKind},
    config::{CiLaneConfig, CiProjectConfig},
};

/// Qualified physical bindings owned by one local executor fleet.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Deployment {
    revision: String,
    pools: BTreeMap<String, PoolBinding>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PoolBinding {
    platform: Platform,
    trust: CacheTrust,
    labels: Vec<String>,
    prepared: bool,
    kinds: Vec<PipelineKind>,
}

/// Project one fleet's frozen admissions onto its local executor bindings.
///
/// The view is checked against the pinned public catalog first; nothing is
/// selected again. A fleet without admissions needs no deployment.
pub(super) fn project(
    view: &PlanViewV3,
    catalog: &Catalog,
    config: &CiProjectConfig,
    deployment: Option<&Deployment>,
    fleet: Fleet,
) -> Result<BTreeMap<String, Value>> {
    verify_catalog(view, catalog, config)?;
    let admissions = match fleet {
        Fleet::Github => &view.providers.github,
        Fleet::Gitlab => &view.providers.gitlab,
    };
    if admissions.is_empty() {
        return Ok(BTreeMap::new());
    }
    let deployment =
        deployment.context("nonempty admission requires a protected local deployment binding")?;
    ensure!(
        deployment.revision == view.request.identity.policy_revision,
        "deployment policy revision differs"
    );
    let kind: PipelineKind = serde_json::from_value(json!(view.request.kind))?;
    let trust: CacheTrust = serde_json::from_value(json!(view.operation.logical_trust))?;
    let mut jobs = BTreeMap::new();
    for admission in admissions {
        let check = &view.checks[&admission.check];
        let binding = deployment
            .pools
            .get(&admission.pool)
            .context("admitted pool has no protected deployment binding")?;
        ensure!(
            binding.platform.os == check.platform.os
                && binding.platform.arch == check.platform.arch,
            "deployment platform differs from the check"
        );
        ensure!(
            binding.trust == trust,
            "deployment cache trust differs from the logical trust"
        );
        ensure!(
            binding.kinds.contains(&kind),
            "deployment is not qualified for this context"
        );
        ensure!(
            !binding.labels.is_empty() && binding.labels.iter().all(|label| !label.is_empty()),
            "deployment has no executor labels"
        );
        ensure!(
            check
                .needs
                .iter()
                .all(|need| admissions.iter().any(|owner| &owner.check == need)),
            "cross-provider artifact dependency has no qualified transport"
        );
        ensure!(
            view.request
                .parameters
                .get(&check.definition)
                .is_none_or(BTreeMap::is_empty),
            "parameterized check requires its command adapter"
        );
        let lane = config.lanes.get(&check.definition);
        let job = match fleet {
            Fleet::Gitlab => native_job(
                view,
                check,
                lane.context("check requires its dedicated native adapter")?,
                binding,
                kind,
                native_artifacts(catalog, &check.definition, binding)?,
            )?,
            Fleet::Github => hosted_entry(view, check, lane, binding, kind)?,
        };
        jobs.insert(admission.check.clone(), job);
    }
    Ok(jobs)
}

fn hosted_entry(
    view: &PlanViewV3,
    check: &wire::Check,
    lane: Option<&CiLaneConfig>,
    binding: &PoolBinding,
    kind: PipelineKind,
) -> Result<Value> {
    ensure!(binding.prepared, "hosted lane requires a prepared executor");
    let recipe = &view.graph.definitions[&check.definition].recipe;
    ensure!(
        recipe.family == "shared-lane" && lane.is_some(),
        "check requires its dedicated hosted adapter"
    );
    Ok(json!({
        "lane":check.definition,"role":recipe.role,"timeout":check.timeout_minutes,
        "tree":check.expected_tree,"source":view.operation.execution[&check.purpose].commit,
        "kind":kind.name(),"history":lane.is_some_and(|lane| lane.history),
        "artifact":recipe.artifact,"labels":binding.labels,"trust":binding.trust,
        "narrow":view.request.narrow,"pr-tests":view.request.draft_tests,"needs":check.needs
    }))
}

fn native_artifacts(
    catalog: &Catalog,
    definition: &str,
    binding: &PoolBinding,
) -> Result<Option<Value>> {
    let mut contract = None;
    for (entrypoint, targets) in &catalog.bindings {
        if !entrypoint.starts_with(".gitlab/ci/") || !targets.contains(definition) {
            continue;
        }
        ensure!(
            binding.prepared || targets.len() == 1,
            "compound native host job requires its existing adapter"
        );
        if targets.len() != 1 {
            continue;
        }
        let (path, job) = entrypoint
            .split_once("::")
            .context("native entrypoint identity")?;
        let document: serde_yaml_ng::Value =
            serde_yaml_ng::from_slice(&fs::read(catalog.root.join(path))?)?;
        let Some(artifacts) = document[job].get("artifacts") else {
            continue;
        };
        let artifacts = serde_json::to_value(artifacts)?;
        ensure!(
            artifacts["when"] == "always",
            "native conditional artifact contract requires its staging adapter"
        );
        ensure!(
            contract
                .as_ref()
                .is_none_or(|existing| existing == &artifacts),
            "native report variants require an explicit qualified binding"
        );
        contract = Some(artifacts);
    }
    Ok(contract)
}

fn quoted(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn native_job(
    view: &PlanViewV3,
    check: &wire::Check,
    lane: &CiLaneConfig,
    binding: &PoolBinding,
    kind: PipelineKind,
    inherited_artifacts: Option<Value>,
) -> Result<Value> {
    let source = view
        .operation
        .execution
        .get(&check.purpose)
        .context("native check has no frozen execution source")?;
    ensure!(
        lane.semantics
            .as_ref()
            .is_some_and(|semantics| semantics.acceptance == "native-exit"),
        "native acceptance requires its existing verdict adapter"
    );
    ensure!(
        lane.queue.is_none(),
        "native resource-group binding requires its existing adapter"
    );
    let wrapper = if binding.prepared { "lane" } else { "run" };
    let mut command = format!(
        "just ci {wrapper} {} --kind {}",
        quoted(&check.definition),
        quoted(kind.name())
    );
    if let Some(narrow) = &view.request.narrow {
        command.push_str(&format!(" --narrow {}", quoted(narrow)));
    }
    let mut artifacts = inherited_artifacts.unwrap_or_else(|| json!({"when":"always","paths":[]}));
    let mut paths = artifacts["paths"]
        .as_array()
        .context("native artifact paths")?
        .clone();
    let cache = json!(".ci-artifacts/cache/gitlab-$CI_JOB_ID-*.json");
    if !paths.contains(&cache) {
        paths.push(cache);
    }
    let mut after = Vec::new();
    if let Some(artifact) = &lane.artifact {
        ensure!(
            artifact.report.is_none(),
            "native report requires its capture adapter"
        );
        ensure!(
            !artifact.wanted(false),
            "native success evidence requires its capture adapter"
        );
        let path = artifact
            .path
            .as_deref()
            .context("native path artifact missing")?;
        paths.retain(|value| {
            value
                .as_str()
                .is_none_or(|existing| existing.trim_end_matches('/') != path.trim_end_matches('/'))
        });
        ensure!(
            !path.contains(['*', '?', '[', ']', '\n', '\r']),
            "native artifact requires its path adapter"
        );
        ensure!(
            std::path::Path::new(path)
                .components()
                .all(|part| matches!(part, std::path::Component::Normal(_))),
            "native artifact must be a relative path"
        );
        let report = format!(
            ".ci-artifacts/planned-reports/$CI_JOB_ID/{}",
            digest(check.definition.as_bytes())
        );
        after.push(format!(
            "case \"$CI_JOB_ID\" in ''|*[!0-9]*) exit 1;; esac\ncase \"$CI_JOB_STATUS\" in failed)\nif [ -e {} ]; then\ntest ! -L {}\nmkdir -p \".ci-artifacts/planned-reports/$CI_JOB_ID\"\nmv -- {} \"{report}\"\nfi\n;; esac",
            quoted(path), quoted(path), quoted(path)
        ));
        paths.push(json!(".ci-artifacts/planned-reports/$CI_JOB_ID/"));
    }
    let mut variables = json!({"KITHARA_PR_TESTS":{"value":view.request.draft_tests.as_deref().unwrap_or(""),"expand":false}});
    if lane.history {
        variables["GIT_DEPTH"] = json!("0");
    }
    artifacts["paths"] = json!(paths);
    Ok(json!({
        "stage":"validation","tags":binding.labels,"timeout":format!("{} minutes",check.timeout_minutes),
        "retry":0,"allow_failure":false,
        "variables":variables,
        "needs":check.needs.iter().map(|need| json!({"job":need,"artifacts":true})).collect::<Vec<_>>(),
        "before_script":[format!("test \"$KITHARA_CACHE_TRUST\" = {}",quoted(binding.trust.as_str())),
            format!("git fetch --quiet origin {}",quoted(&source.commit)),
            format!("git checkout --quiet --detach {}",quoted(&source.commit)),
            format!("test \"$(git rev-parse 'HEAD^{{tree}}')\" = {}",quoted(&check.expected_tree)),
            "git diff --quiet HEAD --"],
        "script":[command],"after_script":after,"artifacts":artifacts
    }))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use kithara_devtools::Ctx;
    use serde_json::{Value, json};

    use super::*;
    use crate::{
        ci::lane::plan::{
            prepare_view,
            request::{PlanRequest, PlanningPolicy, Purpose},
            tests::{plan_request, policy},
        },
        config::KitharaExt,
        testing::workspace::root,
    };

    struct Fixture {
        ctx: Ctx,
        catalog: Catalog,
        config: CiProjectConfig,
    }

    impl Fixture {
        fn new() -> Self {
            let root = root();
            let ctx = Ctx::new(
                root.clone(),
                kithara_devtools::common::project::ProjectConfig::load(&root).unwrap(),
            );
            let config = KitharaExt::from_ctx(&ctx).unwrap().ci;
            let catalog = Catalog::load(&root, &config).unwrap();
            Self {
                ctx,
                catalog,
                config,
            }
        }

        fn placement(&self, request: &PlanRequest, provider: Fleet) -> PlanningPolicy {
            let mut placement = policy(&self.catalog, request);
            placement
                .platforms
                .retain(|platform| platform.os == "linux");
            placement
                .pools
                .get_mut("pool-linux-x86-64")
                .unwrap()
                .provider = provider;
            placement
        }

        fn view(
            &self,
            request: PlanRequest,
            placement: &PlanningPolicy,
            trust: &str,
        ) -> Result<PlanViewV3> {
            let operation: wire::Operation = serde_json::from_value(
                json!({"key":"test-operation","attempt":1,
                "predecessor_status_id":null,"original_kind":request.kind.name(),"event_kind":"push",
                "logical_trust":trust,"scope":"draft",
                "head_repository":{"id":2,"name":"source/project"},"status_repository":{"id":1,"name":"zvuk/kithara"},
                "execution":{"revision":{"repository":{"id":2,"name":"source/project"},"commit":request.identity.head,"tree":request.identity.expected_trees[&Purpose::Revision]}}}),
            )?;
            prepare_view(&self.ctx, request, placement, &operation, &"e".repeat(40))
        }

        fn project(
            &self,
            view: &PlanViewV3,
            deployment: Option<&Deployment>,
            fleet: Fleet,
        ) -> Result<BTreeMap<String, Value>> {
            project(view, &self.catalog, &self.config, deployment, fleet)
        }
    }

    fn request() -> PlanRequest {
        let mut request = plan_request(&["linux-lint", "linux-tooling"]);
        request.identity.expected_trees = BTreeMap::from([(Purpose::Revision, "d".repeat(40))]);
        request
    }

    fn bindings(revision: &str) -> Deployment {
        serde_json::from_value(json!({
            "revision":revision,
            "pools":{"pool-linux-x86-64":{
                "platform":{"os":"linux","arch":"x86_64"},
                "trust":"review","labels":["test-linux"],
                "prepared":true,"kinds":["branch","merge-request"]
            }}
        }))
        .unwrap()
    }

    fn native(request: PlanRequest) -> BTreeMap<String, Value> {
        let fixture = Fixture::new();
        let placement = fixture.placement(&request, Fleet::Gitlab);
        let view = fixture.view(request, &placement, "review").unwrap();
        let deployment = bindings(&placement.revision);
        fixture
            .project(&view, Some(&deployment), Fleet::Gitlab)
            .unwrap()
    }

    #[test]
    fn disabled_required_pool_cannot_produce_a_view() {
        let fixture = Fixture::new();
        let request = request();
        let mut placement = fixture.placement(&request, Fleet::Gitlab);
        placement
            .pools
            .get_mut("pool-linux-x86-64")
            .unwrap()
            .enabled = false;
        let result = fixture.view(request, &placement, "review");
        assert!(format!("{:#}", result.err().unwrap()).contains("disabled"));
    }

    #[test]
    fn a_fleet_without_admissions_needs_no_deployment() {
        let fixture = Fixture::new();
        let request = request();
        let placement = fixture.placement(&request, Fleet::Gitlab);
        let view = fixture.view(request, &placement, "review").unwrap();
        assert!(
            fixture
                .project(&view, None, Fleet::Github)
                .unwrap()
                .is_empty()
        );
        assert!(fixture.project(&view, None, Fleet::Gitlab).is_err());
        assert_eq!(view.checks.len(), 2);
    }

    #[test]
    fn hosted_entries_consume_frozen_contracts_without_selecting_again() {
        let fixture = Fixture::new();
        let request = request();
        let placement = fixture.placement(&request, Fleet::Github);
        let view = fixture.view(request, &placement, "review").unwrap();
        let deployment = bindings(&placement.revision);
        assert!(fixture.project(&view, None, Fleet::Github).is_err());
        let entries = fixture
            .project(&view, Some(&deployment), Fleet::Github)
            .unwrap();
        let lint = &entries["linux-lint@revision/linux/x86_64"];
        assert_eq!(lint["lane"], "linux-lint");
        assert_eq!(lint["timeout"], 60);
        assert_eq!(lint["tree"], "d".repeat(40));
        assert_eq!(lint["source"], "a".repeat(40));
        assert_eq!(lint["labels"], json!(["test-linux"]));
        assert_eq!(lint["trust"], "review");
        let mut changed = fixture.view(self::request(), &placement, "review").unwrap();
        changed.attestation.catalog_digest = "0".repeat(64);
        assert!(
            fixture
                .project(&changed, Some(&deployment), Fleet::Github)
                .is_err()
        );
    }

    #[test]
    fn quarantine_cannot_use_review_deployment_even_in_branch_context() {
        let fixture = Fixture::new();
        let mut request = request();
        request.kind = PipelineKind::Branch;
        let placement = fixture.placement(&request, Fleet::Gitlab);
        let view = fixture.view(request, &placement, "quarantine").unwrap();
        let deployment = bindings(&placement.revision);
        assert!(
            fixture
                .project(&view, Some(&deployment), Fleet::Gitlab)
                .is_err()
        );
    }

    #[test]
    fn deployment_cannot_supply_parallel_enabled_state() {
        let mut value = serde_json::to_value(bindings(&"c".repeat(40))).unwrap();
        value["pools"]["pool-linux-x86-64"]["enabled"] = Value::Bool(true);
        assert!(serde_json::from_value::<Deployment>(value).is_err());
    }

    #[test]
    fn native_definitions_keep_canonical_budgets_narrowing_and_cache_artifacts() {
        let jobs = native(request());
        let tooling = &jobs["linux-tooling@revision/linux/x86_64"];
        assert_eq!(tooling["timeout"], "180 minutes");
        assert_eq!(tooling["retry"], 0);
        assert_eq!(tooling["allow_failure"], false);
        assert_eq!(tooling["artifacts"]["when"], "always");
        assert!(tooling["variables"].get("KITHARA_CACHE_TRUST").is_none());
        let mut narrowed = request();
        narrowed.only = ["linux-tooling".to_owned()].into_iter().collect();
        narrowed.narrow = Some("test(ci::lane::plan::)".to_owned());
        let narrowed = native(narrowed);
        let command = narrowed["linux-tooling@revision/linux/x86_64"]["script"][0]
            .as_str()
            .unwrap();
        assert!(command.contains("--narrow 'test(ci::lane::plan::)'"));
        let paths = tooling["artifacts"]["paths"].as_array().unwrap();
        assert!(paths.contains(&json!(".ci-artifacts/cache/gitlab-$CI_JOB_ID-*.json")));
        assert!(!paths.contains(&json!(".ci-artifacts/hang")));
        assert_eq!(
            jobs["linux-lint@revision/linux/x86_64"]["timeout"],
            "60 minutes"
        );
    }

    #[cfg(unix)]
    #[test]
    fn native_checkout_executes_the_frozen_source_and_preserves_dirty_files() {
        let directory = tempfile::tempdir().unwrap();
        let checkout = directory.path().join("checkout");
        fs::create_dir(&checkout).unwrap();
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args([
                    "-c",
                    "user.name=native-fixture",
                    "-c",
                    "user.email=native-fixture@example.invalid",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .current_dir(&checkout)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .output()
                .unwrap();
            assert!(output.status.success(), "{args:?}: {output:?}");
            String::from_utf8(output.stdout).unwrap().trim().to_owned()
        };
        git(&["init", "--quiet"]);
        fs::write(checkout.join("source.txt"), "source\n").unwrap();
        git(&["add", "--all"]);
        git(&["commit", "--quiet", "--message", "source"]);
        let source = git(&["rev-parse", "HEAD"]);
        let tree = git(&["rev-parse", "HEAD^{tree}"]);
        fs::write(checkout.join("controller.json"), "{}\n").unwrap();
        git(&["add", "--all"]);
        git(&["commit", "--quiet", "--message", "controller"]);
        git(&["remote", "add", "origin", "."]);
        let mut request = request();
        request.only = ["linux-tooling".to_owned()].into_iter().collect();
        request.identity.head = source.clone();
        request.identity.expected_trees = BTreeMap::from([(Purpose::Revision, tree)]);
        let jobs = native(request);
        let before = jobs["linux-tooling@revision/linux/x86_64"]["before_script"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        let marker = directory.path().join("executed");
        let script = format!(
            "{before}\nprintf ready > {}",
            quoted(&marker.to_string_lossy())
        );
        let execute = || {
            std::process::Command::new("bash")
                .args(["-euc", &script])
                .current_dir(&checkout)
                .env("KITHARA_CACHE_TRUST", "review")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .output()
                .unwrap()
        };
        let result = execute();
        assert!(result.status.success(), "{result:?}");
        assert_eq!(git(&["rev-parse", "HEAD"]), source);
        assert!(marker.is_file());
        fs::remove_file(&marker).unwrap();
        fs::write(checkout.join("source.txt"), "dirty\n").unwrap();
        assert!(!execute().status.success());
        assert!(!marker.exists());
        assert_eq!(
            fs::read_to_string(checkout.join("source.txt")).unwrap(),
            "dirty\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn native_hang_staging_is_failure_only_while_cache_remains_current() {
        let jobs = native(request());
        let job = &jobs["linux-tooling@revision/linux/x86_64"];
        let script = job["after_script"][0].as_str().unwrap();
        for status in ["success", "failed"] {
            let directory = tempfile::tempdir().unwrap();
            let artifacts = directory.path().join(".ci-artifacts");
            fs::create_dir_all(artifacts.join("hang")).unwrap();
            fs::create_dir_all(artifacts.join("cache")).unwrap();
            fs::write(artifacts.join("hang/report.txt"), "private-failure-report").unwrap();
            let cache = artifacts.join("cache/gitlab-7-tooling.json");
            fs::write(&cache, "current-job-cache").unwrap();
            assert!(
                std::process::Command::new("bash")
                    .args(["-c", script])
                    .current_dir(directory.path())
                    .env("CI_JOB_STATUS", status)
                    .env("CI_JOB_ID", "7")
                    .status()
                    .unwrap()
                    .success()
            );
            assert_eq!(fs::read_to_string(cache).unwrap(), "current-job-cache");
            let staged = artifacts.join(format!(
                "planned-reports/7/{}/report.txt",
                digest(b"linux-tooling")
            ));
            assert_eq!(staged.exists(), status == "failed");
        }
    }

    #[test]
    fn native_dependency_audit_retains_the_existing_report_contract() {
        let mut request = request();
        request.only = ["deps-deny".to_owned()].into_iter().collect();
        let jobs = native(request);
        let job = &jobs["deps-deny@revision/linux/x86_64"];
        assert_eq!(job["timeout"], "30 minutes");
        assert_eq!(job["artifacts"]["when"], "always");
        let paths = job["artifacts"]["paths"].as_array().unwrap();
        assert!(paths.contains(&json!(".ci-artifacts/deps-deny/")));
        assert!(paths.contains(&json!(".ci-artifacts/cache/gitlab-$CI_JOB_ID-*.json")));
        assert_eq!(job["artifacts"]["expire_in"], "30 days");
    }
}

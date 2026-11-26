use std::{
    collections::{BTreeMap, BTreeSet},
    io,
};

use super::{
    HarnessChoice, MAX_VIEW_BYTES, PlanViewV3, ROLES, ReadPins, SCHEMA_DIGEST, canonical_json,
    require,
};

pub(super) fn hash(value: &str, width: usize) -> bool {
    value.len() == width
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_:.-".contains(&byte))
}

pub(super) fn relative(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('/')
        && !value.contains(['\\', ':'])
        && value
            .split('/')
            .all(|part| !matches!(part, "" | "." | ".."))
        && !value.chars().any(char::is_control)
}

fn repository(value: &str) -> bool {
    let parts: Vec<_> = value.split('/').collect();
    parts.len() == 2
        && parts.iter().all(|part| {
            !matches!(*part, "" | "." | "..")
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
        })
}

fn purpose(value: &str) -> bool {
    matches!(value, "source-head" | "merge" | "revision")
}

/// Require every dependency to name a known node and the graph to have no cycle.
///
/// # Errors
///
/// Fails on an unknown or self dependency and on a cycle.
pub fn validate_acyclic<'a, I>(dependencies: I) -> io::Result<()>
where
    I: IntoIterator<Item = (&'a str, Vec<&'a str>)>,
{
    let nodes: BTreeMap<_, _> = dependencies.into_iter().collect();
    require(
        nodes.iter().all(|(id, needs)| {
            needs
                .iter()
                .all(|need| nodes.contains_key(need) && id != need)
        }),
        "unknown dependency or self cycle",
    )?;
    let mut remaining: BTreeSet<_> = nodes.keys().copied().collect();
    while !remaining.is_empty() {
        let ready: Vec<_> = remaining
            .iter()
            .filter(|id| nodes[*id].iter().all(|need| !remaining.contains(need)))
            .copied()
            .collect();
        require(!ready.is_empty(), "plan contains a dependency cycle")?;
        for id in ready {
            remaining.remove(id);
        }
    }
    Ok(())
}

impl PlanViewV3 {
    /// Check the fixed view shape against the reader pins.
    ///
    /// # Errors
    ///
    /// Fails on the first violated shape or identity rule.
    pub fn validate(&self, pins: &ReadPins<'_>) -> io::Result<()> {
        require(
            self.version == 3
                && self.schema_digest == pins.schema_digest
                && pins.schema_digest == SCHEMA_DIGEST
                && self.consumer_source_pin == pins.consumer_source_pin
                && hash(&self.schema_digest, 64)
                && hash(&self.consumer_source_pin, 40),
            "view source/schema pin",
        )?;
        let request = &self.request;
        require(
            identifier(&request.kind)
                && request.only.iter().all(|name| identifier(name))
                && request
                    .changed_paths
                    .as_ref()
                    .is_none_or(|paths| paths.iter().all(|path| relative(path)))
                && (!request.full
                    || (request.only.is_empty()
                        && request.narrow.is_none()
                        && request.draft_tests.is_none())),
            "request scope",
        )?;
        require(
            request.parameters.iter().all(|(name, values)| {
                self.graph.definitions.get(name).is_some_and(|definition| {
                    values.keys().all(|key| definition.parameters.contains(key))
                })
            }),
            "declared manual parameters",
        )?;
        let identity = &request.identity;
        require(
            repository(&identity.repository)
                && [&identity.head, &identity.base, &identity.policy_revision]
                    .iter()
                    .all(|value| hash(value, 40)),
            "verification identity",
        )?;
        let operation = &self.operation;
        require(
            identifier(&operation.key)
                && operation.attempt > 0
                && operation.predecessor_status_id != Some(0),
            "operation identity",
        )?;
        require(
            repository(&operation.head_repository.name)
                && repository(&operation.status_repository.name)
                && identity.repository == operation.head_repository.name,
            "operation repositories",
        )?;
        require(
            matches!(
                operation.logical_trust.as_str(),
                "review" | "quarantine" | "trusted"
            ) && matches!(operation.scope.as_str(), "draft" | "full")
                && matches!(
                    operation.event_kind.as_str(),
                    "push" | "merge_request_event" | "api" | "web" | "schedule"
                )
                && matches!(
                    operation.original_kind.as_str(),
                    "branch"
                        | "main"
                        | "merge-request"
                        | "quarantine"
                        | "nightly"
                        | "weekly"
                        | "bench"
                        | "platforms"
                        | "release"
                        | "provision"
                ),
            "logical operation context",
        )?;
        require(
            !identity.expected_trees.is_empty()
                && identity
                    .expected_trees
                    .keys()
                    .eq(operation.execution.keys()),
            "used purpose subset",
        )?;
        for (name, tree) in &identity.expected_trees {
            let execution = &operation.execution[name];
            require(
                purpose(name)
                    && hash(tree, 40)
                    && execution.tree == *tree
                    && hash(&execution.commit, 40)
                    && repository(&execution.repository.name),
                "execution purpose/tree",
            )?;
        }
        require(
            [
                &self.attestation.catalog_digest,
                &self.attestation.contract_digest,
                &self.attestation.generation,
                &self.attestation.raw_plan_sha256,
            ]
            .iter()
            .all(|value| hash(value, 64)),
            "opaque plan attestation",
        )?;
        require(!self.checks.is_empty(), "empty selected checks")?;
        self.validate_graph()?;
        let mut counts = BTreeMap::<&str, usize>::new();
        for rows in [&self.providers.github, &self.providers.gitlab] {
            let mut seen = BTreeSet::new();
            for admission in rows {
                require(
                    self.checks.contains_key(&admission.check)
                        && identifier(&admission.pool)
                        && seen.insert(admission.check.as_str()),
                    "unknown or repeated admission",
                )?;
                *counts.entry(admission.check.as_str()).or_default() += 1;
            }
        }
        let dual: BTreeSet<_> = self
            .attestation
            .dual_checks
            .iter()
            .map(String::as_str)
            .collect();
        require(
            dual.len() == self.attestation.dual_checks.len()
                && dual.iter().all(|id| self.checks.contains_key(*id))
                && counts.len() == self.checks.len()
                && counts
                    .iter()
                    .all(|(id, count)| *count == if dual.contains(id) { 2 } else { 1 }),
            "selected admission coverage",
        )?;
        for (name, choice) in &self.harness_runners {
            require(identifier(name), "harness name")?;
            if let HarnessChoice::Nextest {
                test_threads: Some(threads),
                ..
            } = choice
            {
                require(*threads > 0, "harness threads")?;
            }
        }
        require(canonical_json(self)?.len() <= MAX_VIEW_BYTES, "view size")
    }

    fn validate_graph(&self) -> io::Result<()> {
        require(
            self.graph.roles.len() == ROLES.len(),
            "complete role projection",
        )?;
        for role in ROLES {
            let expected: Vec<_> = self
                .graph
                .definitions
                .iter()
                .filter(|(_, definition)| definition.recipe.role.as_deref() == Some(role))
                .map(|(name, _)| name.clone())
                .collect();
            require(
                self.graph.roles.get(role) == Some(&expected),
                "role projection differs",
            )?;
        }
        for (name, definition) in &self.graph.definitions {
            let recipe = &definition.recipe;
            require(
                identifier(name)
                    && identifier(&definition.kind)
                    && relative(&recipe.owner)
                    && hash(&recipe.source_contract_sha256, 64)
                    && matches!(
                        recipe.family.as_str(),
                        "shared-lane" | "ci-run" | "dedicated"
                    ),
                "definition recipe",
            )?;
            require(
                definition
                    .platforms
                    .iter()
                    .all(|os| matches!(os.as_str(), "linux" | "macos" | "windows"))
                    && definition
                        .kinds
                        .iter()
                        .chain(&definition.capabilities)
                        .chain(&definition.parameters)
                        .all(|value| identifier(value))
                    && definition.timeout_minutes != Some(0),
                "definition metadata",
            )?;
            require(
                recipe
                    .role
                    .as_deref()
                    .is_none_or(|role| ROLES.contains(&role))
                    && recipe
                        .sources
                        .iter()
                        .all(|(path, value)| relative(path) && hash(value, 64)),
                "recipe sources",
            )?;
            let semantics = &recipe.semantics;
            require(
                identifier(&semantics.kind)
                    && identifier(&semantics.acceptance)
                    && semantics
                        .capabilities
                        .iter()
                        .chain(&semantics.reports)
                        .all(|value| identifier(value))
                    && semantics
                        .automatic_kinds
                        .as_ref()
                        .is_none_or(|kinds| kinds.iter().all(|value| identifier(value))),
                "recipe semantics",
            )?;
            if let Some(artifact) = &recipe.artifact {
                require(
                    identifier(&artifact.name)
                        && artifact.paths.iter().all(|path| relative(path))
                        && artifact
                            .report
                            .as_ref()
                            .is_none_or(|report| identifier(report))
                        && matches!(artifact.when.as_str(), "always" | "failure" | "success"),
                    "relative artifact contract",
                )?;
            }
        }
        validate_acyclic(self.graph.definitions.iter().map(|(name, definition)| {
            (
                name.as_str(),
                definition.needs.iter().map(String::as_str).collect(),
            )
        }))?;
        for (entrypoint, targets) in &self.graph.entrypoints {
            let (path, _) = entrypoint
                .split_once("::")
                .ok_or_else(|| io::Error::other("entrypoint shape"))?;
            require(
                relative(path)
                    && targets.iter().all(|target| {
                        self.graph.definitions.contains_key(target)
                            || target == "catalog:all"
                            || target
                                .strip_prefix("catalog:")
                                .is_some_and(|role| ROLES.contains(&role))
                    }),
                "entrypoint reference",
            )?;
        }
        for (id, check) in &self.checks {
            require(
                self.graph.definitions.contains_key(&check.definition)
                    && check.id == *id
                    && *id
                        == format!(
                            "{}@{}/{}/{}",
                            check.definition, check.purpose, check.platform.os, check.platform.arch
                        ),
                "selected check identity",
            )?;
            require(
                purpose(&check.purpose)
                    && self.request.identity.expected_trees.get(&check.purpose)
                        == Some(&check.expected_tree)
                    && check.timeout_minutes > 0
                    && matches!(check.platform.os.as_str(), "linux" | "macos" | "windows")
                    && matches!(check.platform.arch.as_str(), "aarch64" | "x86_64"),
                "selected platform/purpose",
            )?;
            require(
                [
                    &check.source_contract_sha256,
                    &check.build_test_sha256,
                    &check.pins_sha256,
                ]
                .iter()
                .all(|value| hash(value, 64)),
                "selected source digests",
            )?;
        }
        validate_acyclic(self.checks.iter().map(|(id, check)| {
            (
                id.as_str(),
                check.needs.iter().map(String::as_str).collect(),
            )
        }))
    }
}

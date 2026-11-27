use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

use crate::ci::{lane::select::Fleet, run::PipelineKind};

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Platform {
    pub(crate) os: String,
    pub(crate) arch: String,
}

impl Platform {
    pub(super) fn validate(&self) -> Result<()> {
        ensure!(
            matches!(self.os.as_str(), "linux" | "macos" | "windows"),
            "unknown plan OS: {}",
            self.os
        );
        ensure!(
            matches!(self.arch.as_str(), "aarch64" | "x86_64"),
            "unknown plan architecture: {}",
            self.arch
        );
        Ok(())
    }

    pub(super) fn key(&self) -> String {
        format!("{}/{}", self.os, self.arch)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Purpose {
    SourceHead,
    Merge,
    Revision,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct VerificationIdentity {
    pub(crate) repository: String,
    pub(crate) head: String,
    pub(crate) base: String,
    pub(crate) policy_revision: String,
    pub(crate) expected_trees: BTreeMap<Purpose, String>,
}

impl VerificationIdentity {
    pub(super) fn validate(&self) -> Result<()> {
        ensure!(
            self.repository.contains('/')
                && self.repository.split('/').all(|part| !part.is_empty()
                    && part != "."
                    && part != ".."
                    && part
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))),
            "plan source must name a repository, without transport or credentials"
        );
        for revision in [&self.head, &self.base, &self.policy_revision] {
            ensure!(
                git_hash(revision),
                "plan requires full immutable Git identities"
            );
        }
        ensure!(
            !self.expected_trees.is_empty()
                && self.expected_trees.values().all(|tree| git_hash(tree)),
            "plan requires expected execution trees"
        );
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PlanRequest {
    pub(crate) identity: VerificationIdentity,
    pub(crate) kind: PipelineKind,
    #[serde(default)]
    pub(crate) full: bool,
    #[serde(default)]
    pub(crate) only: BTreeSet<String>,
    pub(crate) changed_paths: Option<Vec<String>>,
    pub(crate) narrow: Option<String>,
    pub(crate) draft_tests: Option<String>,
    #[serde(default)]
    pub(crate) parameters: BTreeMap<String, BTreeMap<String, String>>,
}

impl PlanRequest {
    pub(super) fn normalized(mut self) -> Result<Self> {
        self.identity.validate()?;
        ensure!(
            !self.full || (self.narrow.is_none() && self.draft_tests.is_none()),
            "full validation cannot use draft or suite narrowing"
        );
        ensure!(
            self.draft_tests.is_none() || self.kind == PipelineKind::Branch,
            "PR test scope is only valid in a draft context"
        );
        if self.full && !self.kind.is_full() {
            self.kind = PipelineKind::Main;
        }
        if self.kind == PipelineKind::Quarantine {
            self.kind = PipelineKind::MergeRequest;
        }
        ensure!(
            !self.full || self.only.is_empty(),
            "full validation cannot narrow its check inventory"
        );
        if let Some(paths) = &mut self.changed_paths {
            paths.sort();
            paths.dedup();
        }
        if self.full || self.kind.is_full() {
            self.changed_paths = None;
        }
        Ok(self)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Pool {
    pub(crate) provider: Fleet,
    pub(crate) platform: Platform,
    pub(crate) enabled: bool,
    pub(crate) capabilities: BTreeSet<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum PlacementMode {
    Hybrid,
    GitlabOnly,
    DualQualification,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PlanningPolicy {
    pub(crate) revision: String,
    pub(crate) catalog_digest: String,
    pub(crate) mode: PlacementMode,
    pub(crate) platforms: BTreeSet<Platform>,
    pub(crate) pools: BTreeMap<String, Pool>,
    pub(crate) assignments: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub(crate) qualification: BTreeSet<String>,
    #[serde(default)]
    pub(crate) inherited_timeouts: BTreeMap<String, u32>,
}

impl PlanningPolicy {
    pub(super) fn validate(&self) -> Result<()> {
        ensure!(
            git_hash(&self.revision),
            "placement requires an immutable trusted revision"
        );
        ensure!(
            self.catalog_digest.len() == 64
                && self.catalog_digest.bytes().all(|c| c.is_ascii_hexdigit()),
            "placement requires a trusted catalog digest"
        );
        ensure!(
            !self.platforms.is_empty(),
            "trusted policy must declare logical platform axes"
        );
        for platform in &self.platforms {
            platform.validate()?;
        }
        for (name, pool) in &self.pools {
            ensure!(
                !name.is_empty()
                    && name
                        .bytes()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-'),
                "pool IDs must be abstract names without deployment addresses"
            );
            pool.platform.validate()?;
            ensure!(
                pool.capabilities.iter().all(|value| !value.is_empty()
                    && value.bytes().all(|c| c.is_ascii_lowercase()
                        || c.is_ascii_digit()
                        || b"-_:".contains(&c))),
                "pool capabilities must not contain deployment bindings"
            );
        }
        ensure!(
            self.inherited_timeouts.values().all(|minutes| *minutes > 0),
            "timeouts must be positive"
        );
        ensure!(
            self.mode == PlacementMode::DualQualification || self.qualification.is_empty(),
            "dual qualification requires its own placement generation"
        );
        Ok(())
    }
}

fn git_hash(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|c| c.is_ascii_hexdigit())
}

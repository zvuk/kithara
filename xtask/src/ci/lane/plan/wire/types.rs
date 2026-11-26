use std::{collections::BTreeMap, io};

use serde::{Deserialize, Serialize};

pub const SCHEMA_DIGEST: &str = "e735d92b5e0d3d19d8997072e4132fc66de955d1de7330a94c712cadfc35cefb";
pub const MAX_VIEW_BYTES: usize = 1_048_576;
pub const ROLES: [&str; 6] = ["gate", "deep", "platforms", "quality", "release", "mutants"];

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PlanViewV3 {
    pub version: u64,
    pub schema_digest: String,
    pub consumer_source_pin: String,
    pub operation: Operation,
    pub request: Request,
    pub attestation: Attestation,
    pub checks: BTreeMap<String, Check>,
    pub graph: Graph,
    pub harness_runners: BTreeMap<String, HarnessChoice>,
    pub providers: Providers,
}

pub struct ReadPins<'a> {
    pub schema_digest: &'a str,
    pub consumer_source_pin: &'a str,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Repository {
    pub id: u64,
    pub name: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Execution {
    pub repository: Repository,
    pub commit: String,
    pub tree: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Operation {
    pub key: String,
    pub attempt: u64,
    #[serde(deserialize_with = "nullable")]
    pub predecessor_status_id: Option<u64>,
    pub original_kind: String,
    pub event_kind: String,
    pub logical_trust: String,
    pub scope: String,
    pub head_repository: Repository,
    pub status_repository: Repository,
    pub execution: BTreeMap<String, Execution>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub repository: String,
    pub head: String,
    pub base: String,
    pub policy_revision: String,
    pub expected_trees: BTreeMap<String, String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub identity: Identity,
    pub kind: String,
    pub full: bool,
    pub only: Vec<String>,
    #[serde(deserialize_with = "nullable")]
    pub changed_paths: Option<Vec<String>>,
    #[serde(deserialize_with = "nullable")]
    pub narrow: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub draft_tests: Option<String>,
    pub parameters: BTreeMap<String, BTreeMap<String, String>>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Attestation {
    pub catalog_digest: String,
    pub contract_digest: String,
    pub generation: String,
    pub raw_plan_sha256: String,
    pub dual_checks: Vec<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Platform {
    pub os: String,
    pub arch: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Check {
    pub id: String,
    pub definition: String,
    pub purpose: String,
    pub platform: Platform,
    pub expected_tree: String,
    pub timeout_minutes: u64,
    pub needs: Vec<String>,
    pub source_contract_sha256: String,
    pub build_test_sha256: String,
    pub pins_sha256: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Admission {
    pub check: String,
    pub pool: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Providers {
    pub github: Vec<Admission>,
    pub gitlab: Vec<Admission>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub name: String,
    pub paths: Vec<String>,
    #[serde(deserialize_with = "nullable")]
    pub report: Option<String>,
    pub when: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Semantics {
    pub kind: String,
    pub acceptance: String,
    pub capabilities: Vec<String>,
    pub reports: Vec<String>,
    #[serde(deserialize_with = "nullable")]
    pub automatic_kinds: Option<Vec<String>>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Recipe {
    pub family: String,
    pub owner: String,
    pub sources: BTreeMap<String, String>,
    pub source_contract_sha256: String,
    #[serde(deserialize_with = "nullable")]
    pub role: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub artifact: Option<Artifact>,
    pub semantics: Semantics,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Definition {
    pub kind: String,
    pub platforms: Vec<String>,
    pub kinds: Vec<String>,
    pub capabilities: Vec<String>,
    pub needs: Vec<String>,
    pub parameters: Vec<String>,
    #[serde(deserialize_with = "nullable")]
    pub timeout_minutes: Option<u64>,
    pub path_globs: Vec<String>,
    pub recipe: Recipe,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Graph {
    pub definitions: BTreeMap<String, Definition>,
    pub entrypoints: BTreeMap<String, Vec<String>>,
    pub roles: BTreeMap<String, Vec<String>>,
    pub non_validation: String,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "lowercase", deny_unknown_fields)]
pub enum HarnessChoice {
    Nextest {
        #[serde(skip_serializing_if = "Option::is_none")]
        filter: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        ignore_default_filter: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        test_threads: Option<u64>,
    },
    Cargo {
        #[serde(skip_serializing_if = "Option::is_none")]
        doc: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        name_filters: Option<Vec<String>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        no_capture: Option<bool>,
    },
}

fn nullable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::deserialize(deserializer)
}

pub(super) fn require(condition: bool, message: &str) -> io::Result<()> {
    if condition {
        Ok(())
    } else {
        Err(io::Error::other(message))
    }
}

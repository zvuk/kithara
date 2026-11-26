use std::collections::BTreeMap;

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

use super::command::sha256;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Case {
    pub(super) id: String,
    pub(super) parameters: BTreeMap<String, String>,
    pub(super) boundary: Boundary,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Boundary {
    pub(super) setup: String,
    pub(super) action: String,
    pub(super) drop: String,
    pub(super) async_completion: String,
}

#[derive(Serialize)]
pub(super) struct AdmittedCase<'a> {
    pub(super) id: &'a str,
    pub(super) fingerprint: String,
    pub(super) parameters: &'a BTreeMap<String, String>,
    pub(super) boundary: &'a Boundary,
}

impl Case {
    pub(super) fn validate(&self) -> Result<()> {
        ensure!(!self.id.trim().is_empty(), "case ID must not be empty");
        ensure!(
            !self.parameters.is_empty()
                && self
                    .parameters
                    .iter()
                    .all(|(name, value)| !name.trim().is_empty() && !value.trim().is_empty()),
            "case {:?} needs non-empty named parameters",
            self.id
        );
        for (name, value) in [
            ("setup", &self.boundary.setup),
            ("action", &self.boundary.action),
            ("drop", &self.boundary.drop),
            ("async_completion", &self.boundary.async_completion),
        ] {
            ensure!(
                !value.trim().is_empty(),
                "case {:?} needs a non-empty {name} boundary",
                self.id
            );
        }
        Ok(())
    }

    pub(super) fn admitted(&self) -> AdmittedCase<'_> {
        AdmittedCase {
            id: &self.id,
            fingerprint: self.fingerprint(),
            parameters: &self.parameters,
            boundary: &self.boundary,
        }
    }

    fn fingerprint(&self) -> String {
        let canonical = serde_json::to_vec(&(&self.id, &self.parameters, &self.boundary))
            .expect("case declaration serialization is infallible");
        sha256(&canonical)
    }
}

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::ids::EndpointId;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub enum BindingRef {
    Command {
        id: EndpointId,
        #[serde(default)]
        with: BTreeMap<String, String>,
    },
    Parameter {
        id: EndpointId,
        #[serde(default)]
        with: BTreeMap<String, String>,
    },
    Telemetry {
        id: EndpointId,
        #[serde(default)]
        with: BTreeMap<String, String>,
    },
    Model {
        id: EndpointId,
        #[serde(default)]
        with: BTreeMap<String, String>,
    },
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct AdaptivePolicy {
    pub priority: Priority,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[non_exhaustive]
pub enum Priority {
    Required,
    High,
    #[default]
    Normal,
    Low,
}

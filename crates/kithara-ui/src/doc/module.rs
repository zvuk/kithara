use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{
    envelope::{self, DocKind},
    error::UiDocError,
    ids::{ControlKind, DocId, EndpointId, NodeId, SourceUri},
    ron_io,
};

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct ModuleDoc {
    pub schema: String,
    pub version: u32,
    pub id: DocId,
    #[serde(default)]
    pub parameters: Vec<String>,
    pub root: ControlNode,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub enum ControlNode {
    Row {
        #[serde(default)]
        id: Option<NodeId>,
        children: Vec<Self>,
    },
    Column {
        #[serde(default)]
        id: Option<NodeId>,
        children: Vec<Self>,
    },
    Include {
        id: NodeId,
        source: String,
        #[serde(default)]
        with: BTreeMap<String, String>,
    },
    Slot {
        id: NodeId,
        #[serde(default)]
        default: Vec<Self>,
    },
    Control {
        id: NodeId,
        kind: ControlKind,
        #[serde(default)]
        props: BTreeMap<String, PropValue>,
        #[serde(default)]
        read: Option<BindingRef>,
        #[serde(default)]
        write: Option<BindingRef>,
        #[serde(default)]
        adaptive: AdaptivePolicy,
    },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[non_exhaustive]
pub enum PropValue {
    Bool(bool),
    Num(f64),
    Text(String),
}

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
    pub min_w: Option<f32>,
    pub min_h: Option<f32>,
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

/// Parses a validated module document.
///
/// # Errors
/// Returns [`UiDocError`] when the envelope or module body is invalid.
pub fn parse_module(text: &str, origin: &SourceUri) -> Result<ModuleDoc, UiDocError> {
    let envelope = envelope::probe(text, origin)?;
    if envelope.kind != DocKind::Module {
        return Err(UiDocError::WrongDocKind {
            origin: origin.clone(),
            expected: DocKind::Module.name(),
            found: envelope.kind.name(),
        });
    }
    ron_io::options()
        .from_str(text)
        .map_err(|source| UiDocError::Syntax {
            origin: origin.clone(),
            source: Box::new(source),
        })
}

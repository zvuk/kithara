use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::ron_io;
use crate::{
    envelope::{self, DocKind},
    error::UiDocError,
    ids::{ControlKind, DocId, EndpointId, NodeId, SourceUri},
    size::SizeSpec,
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
        #[serde(default)]
        size: Option<SizeSpec>,
        children: Vec<Self>,
    },
    Column {
        #[serde(default)]
        id: Option<NodeId>,
        #[serde(default)]
        size: Option<SizeSpec>,
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
        size: Option<SizeSpec>,
        #[serde(default)]
        default: Vec<Self>,
    },
    Control {
        id: NodeId,
        kind: ControlKind,
        #[serde(default)]
        size: Option<SizeSpec>,
        #[serde(default)]
        props: BTreeMap<String, PropValue<String>>,
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
pub enum PropValue<S = String> {
    Bool(bool),
    Num(f64),
    Text(S),
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

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::size::{Dim, SizeSpec};

    fn origin() -> SourceUri {
        SourceUri("size.kmodule.ron".to_owned())
    }

    #[kithara::test]
    fn control_size_override_parses() {
        let text = r#"(schema: "kithara.module", version: 1, id: "size",
            root: Control(
                id: "x",
                kind: "button",
                size: Some((w: Fixed(40.0), h: Fixed(28.0))),
            ))"#;

        let document = parse_module(text, &origin()).unwrap();
        let ControlNode::Control { size, .. } = document.root else {
            panic!("expected control");
        };

        assert_eq!(
            size,
            Some(SizeSpec::new(Dim::Fixed(40.0), Dim::Fixed(28.0)))
        );
    }

    #[kithara::test]
    fn control_size_defaults_to_none() {
        let text = r#"(schema: "kithara.module", version: 1, id: "size",
            root: Control(id: "x", kind: "button"))"#;

        let document = parse_module(text, &origin()).unwrap();
        let ControlNode::Control { size, .. } = document.root else {
            panic!("expected control");
        };

        assert_eq!(size, None);
    }
}

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::ron_io;
use crate::{
    envelope::{self, DocKind},
    error::UiDocError,
    ids::{DocId, InstanceId, SourceUri},
    size::SizeSpec,
};

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct LayoutDoc {
    pub schema: String,
    pub version: u32,
    pub id: DocId,
    pub root: LayoutNode,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub enum LayoutNode {
    Split {
        axis: Axis,
        children: Vec<SplitChild>,
    },
    Module {
        instance: InstanceId,
        source: String,
        #[serde(default)]
        with: BTreeMap<String, String>,
        #[serde(default)]
        size: Option<SizeSpec>,
        #[serde(default)]
        frame: FrameSides,
        #[serde(default = "default_corners")]
        corners: bool,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct FrameSides {
    #[serde(default = "default_frame_side")]
    pub top: bool,
    #[serde(default = "default_frame_side")]
    pub right: bool,
    #[serde(default = "default_frame_side")]
    pub bottom: bool,
    #[serde(default = "default_frame_side")]
    pub left: bool,
}

impl Default for FrameSides {
    fn default() -> Self {
        Self {
            top: true,
            right: true,
            bottom: true,
            left: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[non_exhaustive]
pub enum Axis {
    Horizontal,
    Vertical,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct SplitChild {
    #[serde(default = "default_weight")]
    pub weight: f32,
    pub node: LayoutNode,
}

fn default_weight() -> f32 {
    1.0
}

fn default_frame_side() -> bool {
    true
}

fn default_corners() -> bool {
    true
}

/// Parses a validated layout document.
///
/// # Errors
/// Returns [`UiDocError`] when the envelope or layout body is invalid.
pub fn parse_layout(text: &str, origin: &SourceUri) -> Result<LayoutDoc, UiDocError> {
    let envelope = envelope::probe(text, origin)?;
    if envelope.kind != DocKind::Layout {
        return Err(UiDocError::WrongDocKind {
            origin: origin.clone(),
            expected: DocKind::Layout.name(),
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

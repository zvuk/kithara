use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::ron_io;
use crate::{
    envelope::{self, DocKind},
    error::UiDocError,
    ids::{DocId, EndpointId, NodeId, SourceUri},
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
        #[serde(default)]
        gap: Option<f32>,
        #[serde(default)]
        pad: Option<f32>,
        children: Vec<Self>,
    },
    Column {
        #[serde(default)]
        id: Option<NodeId>,
        #[serde(default)]
        size: Option<SizeSpec>,
        #[serde(default)]
        gap: Option<f32>,
        #[serde(default)]
        pad: Option<f32>,
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
    DeckHeader {
        id: NodeId,
        #[serde(default)]
        size: Option<SizeSpec>,
        #[serde(default)]
        read: Option<BindingRef>,
        #[serde(default)]
        write: Option<BindingRef>,
        #[serde(default)]
        adaptive: AdaptivePolicy,
        #[serde(default)]
        badge: Option<String>,
    },
    DeckSummary {
        id: NodeId,
        #[serde(default)]
        size: Option<SizeSpec>,
        #[serde(default)]
        read: Option<BindingRef>,
        #[serde(default)]
        write: Option<BindingRef>,
        #[serde(default)]
        adaptive: AdaptivePolicy,
        #[serde(default)]
        style: DeckSummaryStyle,
    },
    Brand {
        id: NodeId,
        #[serde(default)]
        size: Option<SizeSpec>,
        #[serde(default)]
        read: Option<BindingRef>,
        #[serde(default)]
        write: Option<BindingRef>,
        #[serde(default)]
        adaptive: AdaptivePolicy,
    },
    Spacer {
        id: NodeId,
        #[serde(default)]
        size: Option<SizeSpec>,
        #[serde(default)]
        read: Option<BindingRef>,
        #[serde(default)]
        write: Option<BindingRef>,
        #[serde(default)]
        adaptive: AdaptivePolicy,
    },
    PresetSelector {
        id: NodeId,
        #[serde(default)]
        size: Option<SizeSpec>,
        #[serde(default)]
        read: Option<BindingRef>,
        #[serde(default)]
        write: Option<BindingRef>,
        #[serde(default)]
        adaptive: AdaptivePolicy,
    },
    SettingsButton {
        id: NodeId,
        #[serde(default)]
        size: Option<SizeSpec>,
        #[serde(default)]
        read: Option<BindingRef>,
        #[serde(default)]
        write: Option<BindingRef>,
        #[serde(default)]
        adaptive: AdaptivePolicy,
    },
    Text {
        id: NodeId,
        #[serde(default)]
        size: Option<SizeSpec>,
        #[serde(default)]
        read: Option<BindingRef>,
        #[serde(default)]
        write: Option<BindingRef>,
        #[serde(default)]
        adaptive: AdaptivePolicy,
        #[serde(default)]
        style: TextStyle,
        #[serde(default)]
        label: Option<String>,
    },
    Glyph {
        id: NodeId,
        #[serde(default)]
        size: Option<SizeSpec>,
        #[serde(default)]
        read: Option<BindingRef>,
        #[serde(default)]
        write: Option<BindingRef>,
        #[serde(default)]
        adaptive: AdaptivePolicy,
        icon: IconName,
    },
    NavItem {
        id: NodeId,
        #[serde(default)]
        size: Option<SizeSpec>,
        #[serde(default)]
        read: Option<BindingRef>,
        #[serde(default)]
        write: Option<BindingRef>,
        #[serde(default)]
        adaptive: AdaptivePolicy,
        label: String,
        icon: IconName,
    },
    TabLarge {
        id: NodeId,
        #[serde(default)]
        size: Option<SizeSpec>,
        #[serde(default)]
        read: Option<BindingRef>,
        #[serde(default)]
        write: Option<BindingRef>,
        #[serde(default)]
        adaptive: AdaptivePolicy,
        label: String,
    },
    Button {
        id: NodeId,
        #[serde(default)]
        size: Option<SizeSpec>,
        #[serde(default)]
        read: Option<BindingRef>,
        #[serde(default)]
        write: Option<BindingRef>,
        #[serde(default)]
        adaptive: AdaptivePolicy,
        label: String,
        #[serde(default)]
        active_label: Option<String>,
        #[serde(default)]
        style: ButtonStyle,
    },
    Bpm {
        id: NodeId,
        #[serde(default)]
        size: Option<SizeSpec>,
        #[serde(default)]
        read: Option<BindingRef>,
        #[serde(default)]
        write: Option<BindingRef>,
        #[serde(default)]
        adaptive: AdaptivePolicy,
        #[serde(default)]
        placeholder: Option<String>,
    },
    Time {
        id: NodeId,
        #[serde(default)]
        size: Option<SizeSpec>,
        #[serde(default)]
        read: Option<BindingRef>,
        #[serde(default)]
        write: Option<BindingRef>,
        #[serde(default)]
        adaptive: AdaptivePolicy,
    },
    Scalar {
        id: NodeId,
        #[serde(default)]
        size: Option<SizeSpec>,
        #[serde(default)]
        read: Option<BindingRef>,
        #[serde(default)]
        write: Option<BindingRef>,
        #[serde(default)]
        adaptive: AdaptivePolicy,
        #[serde(default)]
        format: ScalarFormat,
    },
    Fader {
        id: NodeId,
        #[serde(default)]
        size: Option<SizeSpec>,
        #[serde(default)]
        read: Option<BindingRef>,
        #[serde(default)]
        write: Option<BindingRef>,
        #[serde(default)]
        adaptive: AdaptivePolicy,
        #[serde(default)]
        style: FaderStyle,
    },
    Wave {
        id: NodeId,
        #[serde(default)]
        size: Option<SizeSpec>,
        #[serde(default)]
        read: Option<BindingRef>,
        #[serde(default)]
        write: Option<BindingRef>,
        #[serde(default)]
        adaptive: AdaptivePolicy,
        #[serde(default)]
        style: WaveStyle,
    },
    TrackList {
        id: NodeId,
        #[serde(default)]
        size: Option<SizeSpec>,
        #[serde(default)]
        read: Option<BindingRef>,
        #[serde(default)]
        write: Option<BindingRef>,
        #[serde(default)]
        adaptive: AdaptivePolicy,
    },
    Toggle {
        id: NodeId,
        #[serde(default)]
        size: Option<SizeSpec>,
        #[serde(default)]
        read: Option<BindingRef>,
        #[serde(default)]
        write: Option<BindingRef>,
        #[serde(default)]
        adaptive: AdaptivePolicy,
    },
    Checkbox {
        id: NodeId,
        #[serde(default)]
        size: Option<SizeSpec>,
        #[serde(default)]
        read: Option<BindingRef>,
        #[serde(default)]
        write: Option<BindingRef>,
        #[serde(default)]
        adaptive: AdaptivePolicy,
    },
    Readout {
        id: NodeId,
        #[serde(default)]
        size: Option<SizeSpec>,
        #[serde(default)]
        read: Option<BindingRef>,
        #[serde(default)]
        write: Option<BindingRef>,
        #[serde(default)]
        adaptive: AdaptivePolicy,
        #[serde(default)]
        label: Option<String>,
        #[serde(default)]
        tone: Tone,
        #[serde(default)]
        framed: bool,
    },
    Chip {
        id: NodeId,
        #[serde(default)]
        size: Option<SizeSpec>,
        #[serde(default)]
        read: Option<BindingRef>,
        #[serde(default)]
        write: Option<BindingRef>,
        #[serde(default)]
        adaptive: AdaptivePolicy,
        label: String,
    },
    Knob {
        id: NodeId,
        #[serde(default)]
        size: Option<SizeSpec>,
        #[serde(default)]
        read: Option<BindingRef>,
        #[serde(default)]
        write: Option<BindingRef>,
        #[serde(default)]
        adaptive: AdaptivePolicy,
    },
    VuStereo {
        id: NodeId,
        #[serde(default)]
        size: Option<SizeSpec>,
        #[serde(default)]
        read: Option<BindingRef>,
        #[serde(default)]
        write: Option<BindingRef>,
        #[serde(default)]
        adaptive: AdaptivePolicy,
    },
    VuVertical {
        id: NodeId,
        #[serde(default)]
        size: Option<SizeSpec>,
        #[serde(default)]
        read: Option<BindingRef>,
        #[serde(default)]
        write: Option<BindingRef>,
        #[serde(default)]
        adaptive: AdaptivePolicy,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[non_exhaustive]
pub enum IconName {
    Disc,
    Faders,
    Gear,
    Play,
    Playlist,
    SpeakerHigh,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[non_exhaustive]
pub enum DeckSummaryStyle {
    #[default]
    Default,
    Micro,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[non_exhaustive]
pub enum TextStyle {
    #[default]
    Body,
    TrackTitle,
    Section,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[non_exhaustive]
pub enum ButtonStyle {
    #[default]
    Default,
    Transport,
    TransportPrimary,
    MicroPrimary,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[non_exhaustive]
pub enum ScalarFormat {
    #[default]
    Default,
    Percent,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[non_exhaustive]
pub enum FaderStyle {
    #[default]
    Default,
    Volume,
    VolumeCompact,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[non_exhaustive]
pub enum WaveStyle {
    #[default]
    Default,
    Hero,
    Micro,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[non_exhaustive]
pub enum Tone {
    #[default]
    Neutral,
    Accent,
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
            root: Button(
                id: "x",
                label: "PLAY",
                size: Some((w: Fixed(40.0), h: Fixed(28.0))),
            ))"#;

        let document = parse_module(text, &origin()).unwrap();
        let ControlNode::Button { size, .. } = document.root else {
            panic!("expected button");
        };

        assert_eq!(
            size,
            Some(SizeSpec::new(Dim::Fixed(40.0), Dim::Fixed(28.0)))
        );
    }

    #[kithara::test]
    fn control_size_defaults_to_none() {
        let text = r#"(schema: "kithara.module", version: 1, id: "size",
            root: Button(id: "x", label: "PLAY"))"#;

        let document = parse_module(text, &origin()).unwrap();
        let ControlNode::Button { size, .. } = document.root else {
            panic!("expected button");
        };

        assert_eq!(size, None);
    }
}

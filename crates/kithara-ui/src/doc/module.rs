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
    pub title: Option<String>,
    #[serde(default)]
    pub chip: Option<String>,
    #[serde(default)]
    pub chrome: ChromeStyle,
    #[serde(default)]
    pub footer: Option<BindingRef>,
    #[serde(default)]
    pub parameters: Vec<String>,
    pub root: ControlNode,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[non_exhaustive]
pub enum ChromeStyle {
    Full,
    #[default]
    Frame,
    Plain,
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
        #[serde(default)]
        badge: Option<String>,
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
        #[serde(default)]
        columns: Vec<TrackColumn>,
        #[serde(default)]
        columns_state: Option<BindingRef>,
    },
    Tree {
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
    ContextBar {
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
    Segmented {
        id: NodeId,
        #[serde(default)]
        size: Option<SizeSpec>,
        #[serde(default)]
        read: Option<BindingRef>,
        #[serde(default)]
        write: Option<BindingRef>,
        #[serde(default)]
        adaptive: AdaptivePolicy,
        items: Vec<String>,
    },
    Select {
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
    StatusDot {
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
        tone: Tone,
    },
    Cell {
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
        highlighted: bool,
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
        #[serde(default)]
        style: ChipStyle,
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
    Brand,
    DeckLetter,
    TrackTitle,
    Telemetry,
    MicroLabel,
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
pub enum ChipStyle {
    #[default]
    Deck,
    Routing,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[non_exhaustive]
pub enum WaveStyle {
    #[default]
    Default,
    Hero,
    Micro,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[non_exhaustive]
pub enum TrackColumn {
    Index,
    Deck,
    Title,
    Artist,
    Bpm,
    Key,
    Time,
    Energy,
    Transition,
}

impl TrackColumn {
    #[must_use]
    pub const fn endpoint_name(self) -> &'static str {
        match self {
            Self::Index => "index",
            Self::Deck => "deck",
            Self::Title => "title",
            Self::Artist => "artist",
            Self::Bpm => "bpm",
            Self::Key => "key",
            Self::Time => "time",
            Self::Energy => "energy",
            Self::Transition => "transition",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[non_exhaustive]
pub enum Tone {
    #[default]
    Neutral,
    Accent,
    Success,
    Danger,
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

    #[kithara::test]
    fn tree_control_parses_renderer_bindings() {
        let text = r#"(schema: "kithara.module", version: 1, id: "tree",
            root: Tree(
                id: "browser",
                read: Model(id: "library.tree"),
                size: Some((w: Fixed(232.0), h: Fill)),
                adaptive: (priority: Required),
            ))"#;

        let document = parse_module(text, &origin()).unwrap();
        let ControlNode::Tree {
            read,
            size,
            adaptive,
            ..
        } = document.root
        else {
            panic!("expected tree");
        };

        assert!(matches!(read, Some(BindingRef::Model { .. })));
        assert_eq!(size, Some(SizeSpec::new(Dim::Fixed(232.0), Dim::Fill)));
        assert_eq!(adaptive.priority, Priority::Required);
    }

    #[kithara::test]
    fn module_chrome_defaults_to_frame() {
        let text = r#"(schema: "kithara.module", version: 1, id: "frame",
            root: Text(id: "label"))"#;

        let document = parse_module(text, &origin()).unwrap();

        assert_eq!(document.title, None);
        assert_eq!(document.chip, None);
        assert_eq!(document.chrome, ChromeStyle::Frame);
        assert_eq!(document.footer, None);
    }

    #[kithara::test]
    fn full_module_chrome_metadata_parses() {
        let text = r#"(schema: "kithara.module", version: 1, id: "full",
            title: Some("Module title"),
            chip: Some("MOD"),
            chrome: Full,
            footer: Some(Model(id: "module.status")),
            root: Text(id: "label"))"#;

        let document = parse_module(text, &origin()).unwrap();

        assert_eq!(document.title.as_deref(), Some("Module title"));
        assert_eq!(document.chip.as_deref(), Some("MOD"));
        assert_eq!(document.chrome, ChromeStyle::Full);
        assert!(matches!(document.footer, Some(BindingRef::Model { .. })));
    }
}

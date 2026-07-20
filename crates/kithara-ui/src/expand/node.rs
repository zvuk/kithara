use std::collections::BTreeMap;

use crate::{
    error::UiDocError,
    ids::{InternId, SourceUri},
    module::{
        AdaptivePolicy, BindingRef, ButtonStyle, ChipStyle, ChromeStyle, ControlNode,
        DeckSummaryStyle, FaderStyle, IconName, ScalarFormat, TextStyle, Tone, TrackColumn,
        WaveStyle,
    },
    size::SizeSpec,
};

#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum ExpandedNode {
    Row {
        id: Option<InternId>,
        size: Option<SizeSpec>,
        gap: Option<f32>,
        pad: Option<f32>,
        children: Vec<Self>,
    },
    Column {
        id: Option<InternId>,
        size: Option<SizeSpec>,
        gap: Option<f32>,
        pad: Option<f32>,
        children: Vec<Self>,
    },
    Slot {
        id: InternId,
        size: Option<SizeSpec>,
        children: Vec<Self>,
    },
    Control {
        path: InternId,
        id: InternId,
        spec: ControlSpec,
        size: Option<SizeSpec>,
        read: Option<Binding>,
        write: Option<Binding>,
        adaptive: AdaptivePolicy,
    },
}

#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum ControlSpec {
    DeckSummary {
        style: DeckSummaryStyle,
    },
    Brand,
    Spacer,
    PresetSelector,
    SettingsButton,
    Text {
        style: TextStyle,
        label: Option<InternId>,
    },
    Glyph {
        icon: IconName,
    },
    NavItem {
        label: InternId,
        icon: IconName,
    },
    TabLarge {
        label: InternId,
    },
    Button {
        label: InternId,
        active_label: Option<InternId>,
        style: ButtonStyle,
    },
    Bpm {
        placeholder: Option<InternId>,
    },
    Time,
    Scalar {
        format: ScalarFormat,
    },
    Fader {
        style: FaderStyle,
    },
    Wave {
        style: WaveStyle,
        badge: Option<InternId>,
    },
    TrackList {
        columns: Vec<TrackColumn>,
        columns_state: Option<Binding>,
    },
    Tree {
        query: Option<Binding>,
    },
    ContextBar {
        scope_items: Vec<InternId>,
        scope: Option<Binding>,
    },
    Toggle,
    Checkbox,
    Segmented {
        items: Vec<InternId>,
    },
    Select {
        label: InternId,
    },
    StatusDot {
        label: InternId,
        tone: Tone,
    },
    Cell {
        label: Option<InternId>,
        highlighted: bool,
    },
    Readout {
        label: Option<InternId>,
        tone: Tone,
        framed: bool,
    },
    Chip {
        label: InternId,
        style: ChipStyle,
    },
    Knob,
    VuStereo,
    VuVertical,
}

#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Binding {
    Command {
        id: InternId,
        with: BTreeMap<InternId, InternId>,
    },
    Parameter {
        id: InternId,
        with: BTreeMap<InternId, InternId>,
    },
    Telemetry {
        id: InternId,
        with: BTreeMap<InternId, InternId>,
    },
    Model {
        id: InternId,
        with: BTreeMap<InternId, InternId>,
    },
}

#[derive(Debug)]
pub(crate) struct ExpandedModule {
    pub(crate) module: InternId,
    pub(crate) title: Option<InternId>,
    pub(crate) chip: Option<InternId>,
    pub(crate) chrome: ChromeStyle,
    pub(crate) footer: Option<Binding>,
    pub(crate) collapsed: InternId,
    pub(crate) root: ExpandedNode,
}

#[derive(Clone, Copy)]
pub(crate) struct ControlSite<'a> {
    pub(crate) path: &'a str,
    pub(crate) control: &'a ControlNode,
    pub(crate) read: Option<&'a BindingRef>,
    pub(crate) write: Option<&'a BindingRef>,
    pub(crate) columns_state: Option<&'a BindingRef>,
    pub(crate) query: Option<&'a BindingRef>,
    pub(crate) scope: Option<&'a BindingRef>,
}

pub(crate) type ControlVisitor<'v> =
    dyn for<'a> FnMut(ControlSite<'a>, &SourceUri) -> Result<(), UiDocError> + 'v;

pub(crate) struct Budget {
    nodes: usize,
    max: usize,
}

impl Budget {
    pub(crate) fn new(max: usize) -> Self {
        Self { nodes: 0, max }
    }

    pub(crate) fn charge(&mut self, origin: &SourceUri) -> Result<(), UiDocError> {
        self.nodes += 1;
        if self.nodes > self.max {
            return Err(UiDocError::NodesExceeded {
                origin: origin.clone(),
                count: self.nodes,
                max: self.max,
            });
        }
        Ok(())
    }
}

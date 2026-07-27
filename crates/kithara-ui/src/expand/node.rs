use std::collections::BTreeMap;

use crate::{
    error::UiDocError,
    ids::{InternId, SourceUri},
    layout::FrameSides,
    module::{
        AdaptivePolicy, BindingRef, ButtonStyle, ChipStyle, ChromeStyle, ControlNode,
        DeckSummaryStyle, FaderStyle, GlyphStyle, IconName, ScalarFormat, TextAlign, TextStyle,
        Tone, TrackColumn, WaveStyle, WindowControlsStyle,
    },
    size::SizeSpec,
    skin::ColorRole,
};

#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum ExpandedNode {
    Row {
        id: Option<InternId>,
        size: Option<SizeSpec>,
        gap: Option<f32>,
        pad: Option<f32>,
        pad_x: Option<f32>,
        pad_y: Option<f32>,
        frame: Option<FrameSides>,
        background: Option<ColorRole>,
        background_alpha: Option<f32>,
        surface: Option<SurfaceSpec>,
        children: Vec<Self>,
    },
    Column {
        id: Option<InternId>,
        size: Option<SizeSpec>,
        gap: Option<f32>,
        pad: Option<f32>,
        pad_x: Option<f32>,
        pad_y: Option<f32>,
        frame: Option<FrameSides>,
        background: Option<ColorRole>,
        background_alpha: Option<f32>,
        surface: Option<SurfaceSpec>,
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
    Divider,
    PresetSelector,
    SettingsButton,
    WindowDrag,
    TitleBar {
        label: InternId,
    },
    WindowControls {
        style: WindowControlsStyle,
    },
    Text {
        style: TextStyle,
        label: Option<InternId>,
        active: Option<Binding>,
        align: TextAlign,
    },
    Glyph {
        icon: IconName,
        style: GlyphStyle,
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
        icon: Option<IconName>,
        active_label: Option<InternId>,
        style: ButtonStyle,
        frame: Option<FrameSides>,
    },
    Bpm {
        placeholder: Option<InternId>,
    },
    Time,
    Scalar {
        format: ScalarFormat,
        framed: bool,
    },
    Crossfader {
        ticks: bool,
    },
    Fader {
        style: FaderStyle,
        label: Option<InternId>,
    },
    Wave {
        style: WaveStyle,
        badge: Option<InternId>,
        zoom: Option<Binding>,
    },
    Vis,
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
    Swatch {
        role: ColorRole,
        label: InternId,
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
    Knob {
        label: Option<InternId>,
    },
    Meter,
    VuStereo,
    VuVertical {
        ticks: bool,
    },
}

/// Compiled endpoint reference. `id` is the bare endpoint; `key` is the
/// canonical scope-qualified form `<id>@<scope>=<value>[,...]` (equal to `id`
/// when the binding has no scope). Renderers and hosts address reads by `key`.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Binding {
    Command {
        id: InternId,
        key: InternId,
        with: BTreeMap<InternId, InternId>,
    },
    Parameter {
        id: InternId,
        key: InternId,
        with: BTreeMap<InternId, InternId>,
    },
    Telemetry {
        id: InternId,
        key: InternId,
        with: BTreeMap<InternId, InternId>,
    },
    Model {
        id: InternId,
        key: InternId,
        with: BTreeMap<InternId, InternId>,
    },
}

impl Binding {
    #[must_use]
    pub fn id(&self) -> InternId {
        match self {
            Self::Command { id, .. }
            | Self::Parameter { id, .. }
            | Self::Telemetry { id, .. }
            | Self::Model { id, .. } => *id,
        }
    }

    #[must_use]
    pub fn key(&self) -> InternId {
        match self {
            Self::Command { key, .. }
            | Self::Parameter { key, .. }
            | Self::Telemetry { key, .. }
            | Self::Model { key, .. } => *key,
        }
    }
}

#[derive(Debug)]
pub(crate) struct ExpandedModule {
    pub(crate) module: InternId,
    pub(crate) title: Option<InternId>,
    pub(crate) chip: Option<InternId>,
    pub(crate) assign: Vec<InternId>,
    pub(crate) chrome: ChromeStyle,
    pub(crate) footer: Option<Binding>,
    pub(crate) drop: Option<DropSpec>,
    pub(crate) collapsed: InternId,
    pub(crate) root: ExpandedNode,
}

/// Control path a wheel detent publishes on, and the scalar it steps.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct SurfaceSpec {
    pub path: InternId,
    pub write: Binding,
}

/// Compiled drop target of a module: the command the host runs when a drag is
/// released over it, and the flag that reads true while one hovers it.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct DropSpec {
    pub write: Binding,
    pub read: Binding,
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
    pub(crate) zoom: Option<&'a BindingRef>,
    pub(crate) active: Option<&'a BindingRef>,
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

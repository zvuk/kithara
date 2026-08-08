use std::collections::BTreeMap;

use crate::{
    error::UiDocError,
    ids::{InternId, SourceUri},
    layout::FrameSides,
    module::{
        AdaptivePolicy, BindingRef, ButtonStyle, ChipStyle, ChromeStyle, ControlNode,
        DeckSummaryStyle, FaderStyle, GlyphStyle, IconName, PopoverAlign, PopoverAt, ScalarFormat,
        TextAlign, TextStyle, Tone, TrackColumn, WaveStyle, WindowControlsStyle,
    },
    size::{BlockNode, SizeSpec},
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
        active: Option<Binding>,
        active_background: Option<ColorRole>,
        frame_color: Option<ColorRole>,
        active_frame_color: Option<ColorRole>,
        surface: Option<SurfaceSpec>,
        children: Vec<Self>,
    },
    Column {
        id: Option<InternId>,
        size: Option<SizeSpec>,
        gap: Option<f32>,
        align: TextAlign,
        pad: Option<f32>,
        pad_x: Option<f32>,
        pad_y: Option<f32>,
        frame: Option<FrameSides>,
        background: Option<ColorRole>,
        background_alpha: Option<f32>,
        surface: Option<SurfaceSpec>,
        children: Vec<Self>,
    },
    Optional {
        block: BlockSpec,
        child: Box<Self>,
    },
    /// `content` is laid out only inside the overlay, so the node's intrinsic
    /// size is the anchor's alone.
    Popover {
        path: InternId,
        open: Binding,
        at: PopoverAt,
        align: PopoverAlign,
        anchor: Box<Self>,
        content: Box<Self>,
    },
    Pressable {
        path: InternId,
        press: Binding,
        child: Box<Self>,
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
        color: Option<ColorRole>,
        active_color: Option<ColorRole>,
        active: Option<Binding>,
        align: TextAlign,
    },
    Glyph {
        icon: IconName,
        active_icon: Option<IconName>,
        style: GlyphStyle,
        color: Option<ColorRole>,
        active_color: Option<ColorRole>,
        active: Option<Binding>,
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

/// Which side of the host contract a [`Binding`] addresses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum BindingKind {
    Command,
    Parameter,
    Telemetry,
    Model,
}

/// Compiled endpoint reference. `id` is the bare endpoint; `key` is the
/// canonical scope-qualified form `<id>@<scope>=<value>[,...]` (equal to `id`
/// when the binding has no scope). Renderers and hosts address reads by `key`.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct Binding {
    pub with: BTreeMap<InternId, InternId>,
    pub kind: BindingKind,
    pub id: InternId,
    pub key: InternId,
}

#[derive(Debug)]
pub(crate) struct ExpandedModule {
    pub(crate) chrome: ChromeStyle,
    pub(crate) root: ExpandedNode,
    pub(crate) collapsed: InternId,
    pub(crate) module: InternId,
    pub(crate) chip: Option<InternId>,
    pub(crate) drop: Option<DropSpec>,
    pub(crate) footer: Option<Binding>,
    pub(crate) title: Option<InternId>,
    pub(crate) assign: Vec<InternId>,
    pub(crate) includes: Vec<ExpandedInclude>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ExpandedInclude {
    pub(crate) address: Box<[usize]>,
    pub(crate) module: InternId,
}

/// A block the host may hide: the path that addresses it, and the Bool it
/// reads. While that read is true the block is not laid out.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct BlockSpec {
    pub hidden: Binding,
    pub path: InternId,
}

impl BlockNode for ExpandedNode {
    fn block(&self) -> Option<&BlockSpec> {
        match self {
            Self::Optional { block, .. } => Some(block),
            _ => None,
        }
    }
}

/// Control path a wheel detent publishes on, and the scalar it steps.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct SurfaceSpec {
    pub write: Binding,
    pub path: InternId,
}

/// Compiled drop target of a module: the command the host runs when a drag is
/// released over it, and the flag that reads true while one hovers it.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct DropSpec {
    pub read: Binding,
    pub write: Binding,
}

#[derive(Clone, Copy)]
pub(crate) struct ControlSite<'a> {
    pub(crate) control: &'a ControlNode,
    /// Already resolved, so a parameterised list is validated like a literal one.
    pub(crate) columns: &'a [TrackColumn],
    pub(crate) path: &'a str,
    pub(crate) active: Option<&'a BindingRef>,
    pub(crate) columns_state: Option<&'a BindingRef>,
    pub(crate) query: Option<&'a BindingRef>,
    pub(crate) read: Option<&'a BindingRef>,
    pub(crate) scope: Option<&'a BindingRef>,
    pub(crate) write: Option<&'a BindingRef>,
    pub(crate) zoom: Option<&'a BindingRef>,
}

pub(crate) type ControlVisitor<'v> =
    dyn for<'a> FnMut(ControlSite<'a>, &SourceUri) -> Result<(), UiDocError> + 'v;

pub(crate) struct Budget {
    max: usize,
    nodes: usize,
}

impl Budget {
    pub(crate) fn new(max: usize) -> Self {
        Self { max, nodes: 0 }
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

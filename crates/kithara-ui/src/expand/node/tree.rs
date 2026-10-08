use std::collections::BTreeMap;

use crate::{
    error::UiDocError,
    ids::{InternId, SourceUri},
    layout::FrameSides,
    module::{
        BindingRef, ButtonStyle, ChipStyle, ChromeStyle, ControlNode, DeckSummaryStyle, FaderStyle,
        GlyphStyle, IconName, MeasureAxis, Motion, PopoverAlign, PopoverAt, Pose, ScalarFormat,
        TableColumn, TableFrame, TextAlign, TextStyle, Tone, ViewSet, WaveStyle,
        WindowControlsStyle,
    },
    shader::ShaderSpec,
    size::{BlockNode, SizeSpec},
    skin::{ColorRole, FontFamily, FontWeight},
};

#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum ExpandedNode {
    Row {
        id: Option<InternId>,
        size: Option<SizeSpec>,
        measure: Option<MeasureAxis>,
        gap: Option<f32>,
        align: TextAlign,
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
        measure: Option<MeasureAxis>,
        gap: Option<f32>,
        align: TextAlign,
        pad: Option<f32>,
        pad_x: Option<f32>,
        pad_y: Option<f32>,
        frame: Option<FrameSides>,
        frame_color: Option<ColorRole>,
        background: Option<ColorRole>,
        background_alpha: Option<f32>,
        surface: Option<SurfaceSpec>,
        children: Vec<Self>,
    },
    Scroll {
        id: InternId,
        size: Option<SizeSpec>,
        child: Box<Self>,
    },
    /// Offsets what its subtree draws, and nothing else.
    ///
    /// The pose is resolved per frame rather than at compile time, because the
    /// endpoint behind it may move it between one frame and the next. Layout,
    /// addresses, and pointer regions are the child's alone.
    Object {
        pose: Pose,
        to: Option<Pose>,
        phase: Option<Binding>,
        motion: Option<Motion<Binding>>,
        child: Box<Self>,
    },
    /// Draws one branch: the last step whose threshold the measure reaches,
    /// and `base` below the first of them.
    Adaptive {
        measure: MeasureSpec,
        size: Option<SizeSpec>,
        base: Box<Self>,
        steps: Vec<(f32, Self)>,
    },
    /// Laid out while the enclosing container measures a number in `[from,
    /// until)` on the axis it declares.
    Reveal {
        from: f32,
        until: Option<f32>,
        child: Box<Self>,
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
    /// `content` is laid out only inside the overlay, so the node takes no
    /// room in flow.
    Modal {
        path: InternId,
        open: Binding,
        content: Box<Self>,
    },
    Pressable {
        path: InternId,
        press: Binding,
        child: Box<Self>,
    },
    /// Puts its child at a point in the stage that holds it.
    ///
    /// The point moves the child's box, not only what it draws, so the region
    /// that answers the pointer travels with the picture. `write` is where a
    /// drag publishes the point it ends on, and `magnet` names the placements
    /// of the same stage that take it while it is carried.
    Placed {
        path: InternId,
        id: InternId,
        at: (f32, f32),
        read: Option<Binding>,
        write: Option<Binding>,
        magnet: Option<MagnetSpec>,
        child: Box<Self>,
    },
    /// Offers every child the same box, in document order.
    Stage {
        id: InternId,
        size: Option<SizeSpec>,
        children: Vec<Self>,
    },
    /// Selection reserves the largest child size; a list stacks children vertically.
    Slot {
        id: InternId,
        size: Option<SizeSpec>,
        select: bool,
        children: Vec<Self>,
    },
    Control {
        path: InternId,
        id: InternId,
        spec: ControlSpec,
        size: Option<SizeSpec>,
        read: Option<Binding>,
        write: Option<Binding>,
    },
}

#[derive(Clone, Debug, PartialEq, kithara_derive::EnumStr)]
#[enum_str(all = KINDS, method = kind)]
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
        font: Option<FontFamily>,
        weight: Option<FontWeight>,
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
    /// One frame of a named artwork, chosen by how far its reading has run and
    /// how long `seconds` says one pass through the artwork takes.
    Lottie {
        artwork: InternId,
        /// The artwork shown instead while `active` reads true.
        active_artwork: Option<InternId>,
        active: Option<Binding>,
        seconds: f32,
    },
    /// One frame of a named sheet, chosen by how far its reading has run and
    /// how long `seconds` says one pass through the sheet takes.
    Sprite {
        sheet: InternId,
        seconds: f32,
    },
    Shader(ShaderSpec),
    /// Content the toolkit does not own. The kind is the document's word,
    /// resolved against what the application registered by the host that
    /// mounts it.
    Custom {
        kind: InternId,
    },
    PortalMap,
    Range,
    Table {
        columns: Vec<TableColumn>,
        columns_state: Option<Binding>,
        status: Option<Binding>,
        frame: TableFrame,
        width: Option<Binding>,
    },
    Search,
    Tree {
        query: Option<Binding>,
        /// Whether the tree draws a search field: it reads or writes a query.
        search: bool,
        /// Whether a pressed chevron writes apart from its row.
        toggle: bool,
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
        dot_size: Option<f32>,
        tone: Tone,
        active_tone: Option<Tone>,
        active: Option<Binding>,
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
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum BindingKind {
    Command,
    Parameter,
    Telemetry,
    Model,
    /// State the view keeps for itself. `set` is what a write does to it, and
    /// is absent on the side that only reads.
    View {
        set: ViewSet,
        /// The read answers the flag's opposite.
        invert: bool,
    },
    /// One page of a `Tabs` body, by the state that says which page stands. A
    /// read answers whether the state stands at this page, a write stands it
    /// here.
    Page {
        name: InternId,
    },
    /// Tests whether a text read matches `keys`; `invert` negates the result.
    Selects {
        keys: Box<[InternId]>,
        invert: bool,
    },
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
    pub(crate) drop: bool,
    pub(crate) footer: Option<Binding>,
    pub(crate) title: Option<InternId>,
    pub(crate) assign: Vec<InternId>,
    pub(crate) includes: Vec<ExpandedInclude>,
}

/// The path a module's header press publishes on.
pub(crate) fn header_path(instance: &str) -> String {
    format!("{instance}/header")
}

/// The path a module's drop zone publishes on.
pub(crate) fn drop_path(instance: &str) -> String {
    format!("{instance}/drop")
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ExpandedInclude {
    pub(crate) address: Box<[usize]>,
    pub(crate) module: InternId,
}

/// What a placement snaps onto while a drag carries it: the placements of its
/// own stage it names, and how near their centres must come before one of them
/// takes it.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct MagnetSpec {
    pub to: Vec<InternId>,
    pub within: f32,
}

/// A block the host may hide: the path that addresses it, and the Bool it
/// reads. While that read is true the block is not laid out.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct BlockSpec {
    pub hidden: Binding,
    pub path: InternId,
}

/// Where the number that picks a branch comes from: the box the node is given,
/// or a scalar the host answers.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum MeasureSpec {
    Width,
    Height,
    Read(Binding),
}

impl MeasureSpec {
    pub(crate) const fn axis(&self) -> Option<MeasureAxis> {
        match self {
            Self::Width => Some(MeasureAxis::Width),
            Self::Height => Some(MeasureAxis::Height),
            Self::Read(_) => None,
        }
    }

    pub(crate) const fn binding(&self) -> Option<&Binding> {
        match self {
            Self::Read(binding) => Some(binding),
            Self::Width | Self::Height => None,
        }
    }
}

pub(crate) fn adaptive_branch<'a>(
    base: &'a ExpandedNode,
    steps: &'a [(f32, ExpandedNode)],
    value: Option<f32>,
) -> &'a ExpandedNode {
    let Some(value) = value else {
        return base;
    };
    steps
        .iter()
        .rev()
        .find(|(from, _)| *from <= value)
        .map_or(base, |(_, node)| node)
}

impl BlockNode for ExpandedNode {
    fn block(&self) -> Option<&BlockSpec> {
        match self {
            Self::Optional { block, .. } => Some(block),
            _ => None,
        }
    }
}

impl ControlSpec {
    /// Whether this control draws a new picture every frame of its own accord,
    /// with no endpoint and no input involved.
    ///
    /// A visualisation is a picture of a moment rather than of a value: it keeps
    /// its own decay between frames, so a host that stops drawing it stops it.
    /// Everything else changes only when what it reads changes, and is drawn
    /// again then.
    ///
    /// A shader belongs with everything else, not with the visualisation beside
    /// it. It draws exactly what its uniforms say, and every uniform is an
    /// endpoint — so a shader bound to the host's own clock moves by itself and
    /// is caught as a clock reader, and one bound to endpoints that hold still
    /// draws the same picture however often it is asked.
    ///
    /// Spelled out rather than defaulted, so a control added tomorrow stops
    /// compiling here instead of quietly joining the majority.
    pub(crate) const fn paints_every_frame(&self) -> bool {
        match self {
            Self::Vis => true,
            Self::Shader(_)
            | Self::Custom { .. }
            | Self::Bpm { .. }
            | Self::Brand
            | Self::Button { .. }
            | Self::Cell { .. }
            | Self::Checkbox
            | Self::Chip { .. }
            | Self::ContextBar { .. }
            | Self::Crossfader { .. }
            | Self::DeckSummary { .. }
            | Self::Divider
            | Self::Fader { .. }
            | Self::Glyph { .. }
            | Self::Knob { .. }
            | Self::Lottie { .. }
            | Self::Meter
            | Self::NavItem { .. }
            | Self::PortalMap
            | Self::PresetSelector
            | Self::Range
            | Self::Readout { .. }
            | Self::Scalar { .. }
            | Self::Segmented { .. }
            | Self::Select { .. }
            | Self::SettingsButton
            | Self::Spacer
            | Self::Sprite { .. }
            | Self::StatusDot { .. }
            | Self::Swatch { .. }
            | Self::TabLarge { .. }
            | Self::Table { .. }
            | Self::Text { .. }
            | Self::Time
            | Self::TitleBar { .. }
            | Self::Toggle
            | Self::Search
            | Self::Tree { .. }
            | Self::VuStereo
            | Self::VuVertical { .. }
            | Self::Wave { .. }
            | Self::WindowControls { .. }
            | Self::WindowDrag => false,
        }
    }
}

/// What a subtree does with nothing touching it.
///
/// Both halves are asked for together because they are answered by one walk of
/// the same tree, and because a host that separates them ends up with two
/// accounts of when a document moves.
#[derive(Clone, Copy, Default)]
pub(crate) struct Unprompted {
    /// Something here draws a new picture every frame of its own accord, with
    /// no endpoint and no input involved.
    pub(crate) continuous: bool,
    /// Something here is placed by an endpoint rather than by the document
    /// alone, so re-reading the endpoints can move a tree already mounted.
    ///
    /// An object needs both ends of a track and something driving it to move at
    /// all: a far pose nobody travels towards, or a phase with nowhere to carry
    /// the object, both leave it exactly where the document wrote it. A host
    /// asks this to do no pose work on a page that cannot move.
    pub(crate) driven: bool,
}

impl Unprompted {
    const DRIVEN: Self = Self {
        driven: true,
        continuous: false,
    };

    fn of(nodes: &[ExpandedNode]) -> Self {
        nodes.iter().map(motion_of).fold(Self::default(), Self::or)
    }

    pub(crate) fn or(self, other: Self) -> Self {
        Self {
            driven: self.driven || other.driven,
            continuous: self.continuous || other.continuous,
        }
    }
}

/// What one subtree does with nothing touching it.
pub(crate) fn motion_of(node: &ExpandedNode) -> Unprompted {
    match node {
        ExpandedNode::Object {
            to,
            phase,
            motion,
            child,
            ..
        } => {
            let own = if to.is_some() && (phase.is_some() || motion.is_some()) {
                Unprompted::DRIVEN
            } else {
                Unprompted::default()
            };
            own.or(motion_of(child))
        }
        ExpandedNode::Row { children, .. }
        | ExpandedNode::Column { children, .. }
        | ExpandedNode::Stage { children, .. }
        | ExpandedNode::Slot { children, .. } => Unprompted::of(children),
        ExpandedNode::Popover {
            anchor, content, ..
        } => motion_of(anchor).or(motion_of(content)),
        ExpandedNode::Modal { content, .. } => motion_of(content),
        ExpandedNode::Optional { child, .. }
        | ExpandedNode::Placed { child, .. }
        | ExpandedNode::Pressable { child, .. }
        | ExpandedNode::Reveal { child, .. }
        | ExpandedNode::Scroll { child, .. } => motion_of(child),
        ExpandedNode::Adaptive { base, steps, .. } => steps
            .iter()
            .map(|(_, branch)| motion_of(branch))
            .fold(motion_of(base), Unprompted::or),
        ExpandedNode::Control { spec, .. } => Unprompted {
            driven: false,
            continuous: spec.paints_every_frame(),
        },
    }
}

/// Control path a wheel detent publishes on, and the scalar it steps.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct SurfaceSpec {
    pub write: Binding,
    pub path: InternId,
}

#[derive(Clone, Copy)]
pub(crate) struct ControlSite<'a> {
    pub(crate) control: &'a ControlNode,
    /// Already resolved, so a parameterised list is validated like a literal one.
    pub(crate) columns: &'a [TableColumn],
    pub(crate) path: &'a str,
    pub(crate) active: Option<&'a BindingRef>,
    pub(crate) columns_state: Option<&'a BindingRef>,
    pub(crate) status: Option<&'a BindingRef>,
    pub(crate) query: Option<&'a BindingRef>,
    pub(crate) read: Option<&'a BindingRef>,
    pub(crate) scope: Option<&'a BindingRef>,
    pub(crate) write: Option<&'a BindingRef>,
    pub(crate) zoom: Option<&'a BindingRef>,
    pub(crate) writes: SlotWrites<'a>,
    /// What opens the popover a write from this site shuts.
    pub(crate) shuts: Option<&'a BindingRef>,
}

impl<'a> ControlSite<'a> {
    /// A site at `path` that binds nothing.
    pub(crate) fn new(control: &'a ControlNode, path: &'a str) -> Self {
        Self {
            control,
            path,
            columns: &[],
            active: None,
            columns_state: None,
            status: None,
            query: None,
            read: None,
            scope: None,
            write: None,
            zoom: None,
            writes: SlotWrites::default(),
            shuts: None,
        }
    }
}

#[derive(Clone, Copy, Default)]
pub(crate) struct SlotWrites<'a> {
    pub(crate) secondary: Option<&'a BindingRef>,
    pub(crate) reset: Option<&'a BindingRef>,
    pub(crate) zoom: Option<&'a BindingRef>,
    pub(crate) loop_start: Option<&'a BindingRef>,
    pub(crate) loop_end: Option<&'a BindingRef>,
    pub(crate) query: Option<&'a BindingRef>,
    pub(crate) width: Option<&'a BindingRef>,
    pub(crate) toggle: Option<&'a BindingRef>,
}

pub(crate) type ControlVisitor<'v> =
    dyn for<'a> FnMut(ControlSite<'a>, &SourceUri) -> Result<(), UiDocError> + 'v;

pub(crate) struct Budget {
    max: usize,
    nodes: usize,
}

impl Budget {
    pub(crate) const fn new(max: usize) -> Self {
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

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{
    binding::BindingRef,
    measure::{Measure, MeasureAxis},
    motion::{Motion, Pose},
    style::{
        ButtonStyle, ChipStyle, DeckSummaryStyle, FaderStyle, GlyphStyle, IconName, PopoverAlign,
        PopoverAt, PopoverDismiss, ScalarFormat, TableColumn, TextAlign, TextStyle, Tone,
        WaveStyle, WindowControlsStyle,
    },
};
use crate::{
    ids::NodeId,
    layout::FrameSides,
    param::Param,
    size::SizeSpec,
    skin::{ColorRole, FontFamily, FontWeight},
};

const fn default_framed() -> bool {
    true
}

/// A row that says nothing centres its children across itself: a label and a
/// chip of different heights in one row line up on their middles.
const fn centred() -> TextAlign {
    TextAlign::Center
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
        /// Measures the declared box on this axis and reveals the children
        /// whose threshold it reaches.
        #[serde(default)]
        measure: Option<MeasureAxis>,
        #[serde(default)]
        gap: Option<f32>,
        /// Where a child shorter than the row sits across it. Centred unless
        /// the document says otherwise, which is what every row said before
        /// this field existed.
        #[serde(default = "centred")]
        align: TextAlign,
        #[serde(default)]
        pad: Option<f32>,
        /// Per-axis override of `pad`.
        #[serde(default)]
        pad_x: Option<f32>,
        #[serde(default)]
        pad_y: Option<f32>,
        /// Hairlines on the requested sides; absent means no border.
        #[serde(default)]
        frame: Option<FrameSides>,
        /// Fill behind the children; absent means transparent.
        #[serde(default)]
        background: Option<ColorRole>,
        #[serde(default)]
        background_alpha: Option<f32>,
        #[serde(default)]
        active: Option<BindingRef>,
        #[serde(default)]
        active_background: Option<ColorRole>,
        #[serde(default)]
        frame_color: Option<ColorRole>,
        #[serde(default)]
        active_frame_color: Option<ColorRole>,
        #[serde(default)]
        write: Option<BindingRef>,
        /// Written by an activation of the surface its `write` steps.
        #[serde(default)]
        reset: Option<BindingRef>,
        children: Vec<Self>,
    },
    Column {
        #[serde(default)]
        id: Option<NodeId>,
        #[serde(default)]
        size: Option<SizeSpec>,
        /// Measures the declared box on this axis and reveals the children
        /// whose threshold it reaches.
        #[serde(default)]
        measure: Option<MeasureAxis>,
        #[serde(default)]
        gap: Option<f32>,
        #[serde(default)]
        align: TextAlign,
        #[serde(default)]
        pad: Option<f32>,
        /// Per-axis override of `pad`.
        #[serde(default)]
        pad_x: Option<f32>,
        #[serde(default)]
        pad_y: Option<f32>,
        /// Hairlines on the requested sides; absent means no border.
        #[serde(default)]
        frame: Option<FrameSides>,
        #[serde(default)]
        frame_color: Option<ColorRole>,
        /// Fill behind the children; absent means transparent.
        #[serde(default)]
        background: Option<ColorRole>,
        #[serde(default)]
        background_alpha: Option<f32>,
        #[serde(default)]
        write: Option<BindingRef>,
        /// Written by an activation of the surface its `write` steps.
        #[serde(default)]
        reset: Option<BindingRef>,
        children: Vec<Self>,
    },
    Scroll {
        id: NodeId,
        #[serde(default)]
        size: Option<SizeSpec>,
        child: Box<Self>,
    },
    Include {
        id: NodeId,
        source: String,
        #[serde(default)]
        with: BTreeMap<String, String>,
    },
    /// Declares one place in several forms. `measure` picks the last step it
    /// reaches; `base` is the form below every step.
    Adaptive {
        id: NodeId,
        measure: Measure,
        /// Required by a self-measured node and refused by a read one: a box
        /// that came from the branch could not decide which branch to draw.
        #[serde(default)]
        size: Option<SizeSpec>,
        base: Box<Self>,
        steps: Vec<AdaptiveStep>,
    },
    /// Shows its child while the container measures a number in `[from,
    /// until)` on the axis it declares; `until: None` names no ceiling. Only a
    /// container declaring `measure` may hold one.
    Reveal {
        from: f32,
        #[serde(default)]
        until: Option<f32>,
        child: Box<Self>,
    },
    /// Marks its child as a block the host may hide. While `hidden` reads
    /// true the child is not laid out.
    Optional {
        id: NodeId,
        hidden: BindingRef,
        child: Box<Self>,
    },
    /// Floats `content` over the layout while `open` reads true. Only `anchor`
    /// is laid out in flow.
    Popover {
        id: NodeId,
        open: BindingRef,
        #[serde(default)]
        at: PopoverAt,
        #[serde(default)]
        align: PopoverAlign,
        /// `Write` needs `open` to be a view flag.
        #[serde(default)]
        dismiss: PopoverDismiss,
        anchor: Box<Self>,
        content: Box<Self>,
    },
    /// Covers the whole window with a scrim and centres `content` above it
    /// while `open` reads true, taking all input. Escape and a press on the
    /// scrim write `close`. It takes no room in flow.
    Modal {
        id: NodeId,
        open: BindingRef,
        close: BindingRef,
        content: Box<Self>,
    },
    /// Makes its child a click target that publishes on this node's path.
    Pressable {
        id: NodeId,
        press: BindingRef,
        /// Written by a secondary press.
        #[serde(default)]
        secondary: Option<BindingRef>,
        child: Box<Self>,
    },
    /// Offsets its child from wherever its container placed it.
    ///
    /// The base object: any node becomes one by being wrapped, a widget as
    /// readily as a picture, and an object may hold another. The offset moves
    /// what is drawn and nothing else — the container's layout is already
    /// decided by the time this applies, and the region that answers the
    /// pointer stays where that layout put it.
    Object {
        id: NodeId,
        #[serde(default)]
        transform: Pose,
        /// The pose at the far end of the track, if the object travels.
        #[serde(default)]
        to: Option<Pose>,
        /// The scalar that says how far along the track the object is, `0.0`
        /// at `transform` and `1.0` at `to`. One endpoint drives the whole
        /// pose, which is the same scalar a sprite picks its frame by and a
        /// Lottie reads its progress from.
        #[serde(default)]
        phase: Option<BindingRef>,
        /// A duration, a curve and a repeat, run off a clock endpoint, for an
        /// object whose document knows how it moves rather than being told
        /// where it is. This resolves to the same scalar `phase` carries, so an
        /// object declaring both is refused rather than ranked.
        #[serde(default)]
        motion: Option<Motion<BindingRef>>,
        child: Box<Self>,
    },
    /// Puts its child at a point inside the stage that holds it.
    ///
    /// Where an `Object` offsets what is drawn and leaves the layout alone,
    /// this places the child: the box that answers the pointer travels with
    /// the picture, which is what a placement the pointer may carry needs. A
    /// placement with somewhere to write publishes the point a drag leaves it
    /// on, and a magnet pulls that point onto the placements it names.
    Placed {
        id: NodeId,
        /// Where the child stands while nothing else says otherwise.
        #[serde(default)]
        at: (f32, f32),
        /// The point endpoint that says where the child stands, over `at`.
        #[serde(default)]
        read: Option<BindingRef>,
        /// Where a drag publishes the point it ends on. Without one the child
        /// stays wherever it was put.
        #[serde(default)]
        write: Option<BindingRef>,
        /// The placements this one snaps onto while it is carried.
        #[serde(default)]
        magnet: Option<Magnet>,
        child: Box<Self>,
    },
    /// Gives every child the whole box, in document order.
    ///
    /// Where a row or column decides *where* a child goes, this decides
    /// nothing: each child is offered the same box and an `Object` around it
    /// says where inside that box it actually lands. Free placement is the two
    /// together, which is why neither of them needs to know about the other.
    Stage {
        id: NodeId,
        #[serde(default)]
        size: Option<SizeSpec>,
        #[serde(default)]
        children: Vec<Self>,
    },
    /// Slot with defaults or external content; `from` requires exactly one of `each` or `select`.
    /// `each` renders a template per fill; `select` renders the matching key.
    /// Uses `default` when the collection is empty or the key has no match.
    Slot {
        id: NodeId,
        #[serde(default)]
        size: Option<SizeSpec>,
        #[serde(default)]
        default: Vec<Self>,
        #[serde(default)]
        from: Option<String>,
        #[serde(default)]
        each: Option<Include>,
        #[serde(default)]
        select: Option<BindingRef>,
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
    },
    Spacer {
        id: NodeId,
        #[serde(default)]
        size: Option<SizeSpec>,
        #[serde(default)]
        read: Option<BindingRef>,
        #[serde(default)]
        write: Option<BindingRef>,
    },
    /// Horizontal fill bar reporting one scalar.
    Meter {
        id: NodeId,
        #[serde(default)]
        size: Option<SizeSpec>,
        #[serde(default)]
        read: Option<BindingRef>,
        #[serde(default)]
        write: Option<BindingRef>,
    },
    /// Hairline between adjacent bar cells.
    Divider {
        id: NodeId,
        #[serde(default)]
        size: Option<SizeSpec>,
        #[serde(default)]
        read: Option<BindingRef>,
        #[serde(default)]
        write: Option<BindingRef>,
    },
    PresetSelector {
        id: NodeId,
        #[serde(default)]
        size: Option<SizeSpec>,
        #[serde(default)]
        read: Option<BindingRef>,
        #[serde(default)]
        write: Option<BindingRef>,
    },
    SettingsButton {
        id: NodeId,
        #[serde(default)]
        size: Option<SizeSpec>,
        #[serde(default)]
        read: Option<BindingRef>,
        #[serde(default)]
        write: Option<BindingRef>,
    },
    /// Bare drag surface for a window that draws its own chrome.
    WindowDrag {
        id: NodeId,
        #[serde(default)]
        size: Option<SizeSpec>,
    },
    TitleBar {
        id: NodeId,
        label: String,
        #[serde(default)]
        size: Option<SizeSpec>,
    },
    WindowControls {
        id: NodeId,
        #[serde(default)]
        style: WindowControlsStyle,
        #[serde(default)]
        size: Option<SizeSpec>,
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
        style: TextStyle,
        #[serde(default)]
        label: Option<String>,
        /// Where the glyphs sit inside the node's box.
        #[serde(default)]
        align: TextAlign,
        #[serde(default)]
        color: Option<ColorRole>,
        #[serde(default)]
        active_color: Option<ColorRole>,
        #[serde(default)]
        active: Option<BindingRef>,
        /// The face this run is set in, when it is not the one the style names.
        #[serde(default)]
        font: Option<Param<FontFamily>>,
        #[serde(default)]
        weight: Option<Param<FontWeight>>,
    },
    Glyph {
        id: NodeId,
        #[serde(default)]
        size: Option<SizeSpec>,
        #[serde(default)]
        read: Option<BindingRef>,
        #[serde(default)]
        write: Option<BindingRef>,
        icon: Param<IconName>,
        #[serde(default)]
        active_icon: Option<Param<IconName>>,
        #[serde(default)]
        style: GlyphStyle,
        #[serde(default)]
        color: Option<Param<ColorRole>>,
        #[serde(default)]
        active_color: Option<Param<ColorRole>>,
        #[serde(default)]
        active: Option<BindingRef>,
    },
    NavItem {
        id: NodeId,
        #[serde(default)]
        size: Option<SizeSpec>,
        #[serde(default)]
        read: Option<BindingRef>,
        #[serde(default)]
        write: Option<BindingRef>,
        label: String,
        icon: Param<IconName>,
    },
    TabLarge {
        id: NodeId,
        #[serde(default)]
        size: Option<SizeSpec>,
        #[serde(default)]
        read: Option<BindingRef>,
        #[serde(default)]
        write: Option<BindingRef>,
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
        label: String,
        #[serde(default)]
        icon: Option<Param<IconName>>,
        #[serde(default)]
        active_label: Option<String>,
        #[serde(default)]
        style: ButtonStyle,
        /// Hairlines on the requested sides of a transport cell; absent leaves
        /// the sides to the skin.
        #[serde(default)]
        frame: Option<FrameSides>,
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
        format: ScalarFormat,
        #[serde(default = "default_framed")]
        framed: bool,
    },
    Crossfader {
        id: NodeId,
        #[serde(default)]
        size: Option<SizeSpec>,
        #[serde(default)]
        read: Option<BindingRef>,
        #[serde(default)]
        write: Option<BindingRef>,
        /// Draw the scale above the rail.
        #[serde(default)]
        ticks: bool,
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
        style: FaderStyle,
        #[serde(default)]
        label: Option<String>,
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
        style: WaveStyle,
        #[serde(default)]
        badge: Option<String>,
        #[serde(default)]
        zoom: Option<BindingRef>,
        /// Written by the wheel's zoom.
        #[serde(default)]
        write_zoom: Option<BindingRef>,
        /// Where a drag across the wave starts and ends the loop it marks.
        #[serde(default)]
        write_loop_start: Option<BindingRef>,
        #[serde(default)]
        write_loop_end: Option<BindingRef>,
    },
    Vis {
        id: NodeId,
        #[serde(default)]
        size: Option<SizeSpec>,
        #[serde(default)]
        read: Option<BindingRef>,
        #[serde(default)]
        write: Option<BindingRef>,
    },
    /// One frame of a named artwork, chosen by how far its own reading has run.
    ///
    /// The sheet contract with a drawing in place of a picture: `read` hands
    /// over seconds, so binding it to the host's own clock is what makes the
    /// artwork play and binding it to anything else scrubs it by hand;
    /// `seconds` is how long one pass through the whole artwork takes.
    Lottie {
        id: NodeId,
        #[serde(default)]
        size: Option<SizeSpec>,
        artwork: String,
        /// The artwork shown instead while `active` reads true, for a control
        /// whose document answers a press with another drawing.
        #[serde(default)]
        active_artwork: Option<String>,
        #[serde(default)]
        read: Option<BindingRef>,
        #[serde(default)]
        write: Option<BindingRef>,
        /// The flag that says which of the two artworks stands.
        #[serde(default)]
        active: Option<BindingRef>,
        seconds: f32,
    },
    /// One frame of a named sheet, chosen by how far its own reading has run.
    ///
    /// `read` hands over seconds, so binding it to the host's own clock is what
    /// makes the sheet play and binding it to anything else scrubs the sheet by
    /// hand; `seconds` is how long one pass through every frame takes.
    Sprite {
        id: NodeId,
        #[serde(default)]
        size: Option<SizeSpec>,
        sheet: String,
        #[serde(default)]
        read: Option<BindingRef>,
        #[serde(default)]
        write: Option<BindingRef>,
        seconds: f32,
    },
    /// A WGSL fragment fed by named read-only endpoint uniforms.
    Shader {
        id: NodeId,
        #[serde(default)]
        size: Option<SizeSpec>,
        source: String,
        #[serde(default)]
        uniforms: BTreeMap<String, BindingRef>,
    },
    /// Content the toolkit does not own, named by the kind the application
    /// registered it under.
    ///
    /// Nothing here says what it draws: the document names a kind and the host
    /// resolves that kind to a registered widget. A kind nothing registered is
    /// refused while the document compiles, against the set declared to
    /// `UiConfig`, so no host is handed a name it cannot mount. It binds no
    /// endpoint, because the widget behind it has no route to read one; what it
    /// recognises leaves as its own typed action, through the map given at
    /// registration.
    Custom {
        id: NodeId,
        #[serde(default)]
        size: Option<SizeSpec>,
        kind: String,
    },
    PortalMap {
        id: NodeId,
        #[serde(default)]
        size: Option<SizeSpec>,
        #[serde(default)]
        read: Option<BindingRef>,
    },
    Range {
        id: NodeId,
        #[serde(default)]
        size: Option<SizeSpec>,
        #[serde(default)]
        read: Option<BindingRef>,
        #[serde(default)]
        write: Option<BindingRef>,
    },
    Table {
        id: NodeId,
        #[serde(default)]
        size: Option<SizeSpec>,
        #[serde(default)]
        read: Option<BindingRef>,
        #[serde(default)]
        write: Option<BindingRef>,
        #[serde(default)]
        columns: Option<Param<Vec<TableColumn>>>,
        #[serde(default)]
        columns_state: Option<BindingRef>,
        #[serde(default)]
        status: Option<BindingRef>,
        #[serde(default = "default_framed")]
        footer: bool,
        #[serde(default)]
        padding_left: f32,
        #[serde(default)]
        padding_right: f32,
        /// Written with a column's width when its divider is dragged, scoped
        /// by that column's id under `column`.
        #[serde(default)]
        write_width: Option<BindingRef>,
    },
    Search {
        id: NodeId,
        #[serde(default)]
        size: Option<SizeSpec>,
        #[serde(default)]
        read: Option<BindingRef>,
        #[serde(default)]
        write: Option<BindingRef>,
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
        query: Option<BindingRef>,
        /// Written with the text typed into the search field.
        #[serde(default)]
        write_query: Option<BindingRef>,
        /// Written with the row whose chevron is pressed.
        #[serde(default)]
        toggle: Option<BindingRef>,
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
        scope_items: Vec<String>,
        #[serde(default)]
        scope: Option<BindingRef>,
    },
    Toggle {
        id: NodeId,
        #[serde(default)]
        size: Option<SizeSpec>,
        #[serde(default)]
        read: Option<BindingRef>,
        #[serde(default)]
        write: Option<BindingRef>,
    },
    Checkbox {
        id: NodeId,
        #[serde(default)]
        size: Option<SizeSpec>,
        #[serde(default)]
        read: Option<BindingRef>,
        #[serde(default)]
        write: Option<BindingRef>,
    },
    Segmented {
        id: NodeId,
        #[serde(default)]
        size: Option<SizeSpec>,
        #[serde(default)]
        read: Option<BindingRef>,
        #[serde(default)]
        write: Option<BindingRef>,
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
        label: String,
        #[serde(default)]
        dot_size: Option<f32>,
        #[serde(default)]
        tone: Tone,
        #[serde(default)]
        active_tone: Option<Tone>,
        #[serde(default)]
        active: Option<BindingRef>,
    },
    Swatch {
        id: NodeId,
        role: ColorRole,
        label: String,
        #[serde(default)]
        size: Option<SizeSpec>,
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
        label: Option<String>,
    },
    VuStereo {
        id: NodeId,
        #[serde(default)]
        size: Option<SizeSpec>,
        #[serde(default)]
        read: Option<BindingRef>,
        #[serde(default)]
        write: Option<BindingRef>,
    },
    VuVertical {
        id: NodeId,
        #[serde(default)]
        size: Option<SizeSpec>,
        #[serde(default)]
        read: Option<BindingRef>,
        #[serde(default)]
        write: Option<BindingRef>,
        /// Draw the scale left of the fader.
        #[serde(default)]
        ticks: bool,
    },
}

/// Slot item template. Its `content` slot receives the fill document.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct Include {
    pub source: String,
}

/// Adaptive branch selected when the measured value reaches `from`.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct AdaptiveStep {
    pub node: ControlNode,
    pub from: f32,
}

/// What a placement snaps onto while the pointer carries it: the placements in
/// its own stage it names, and how near their centres must come before one of
/// them takes it.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct Magnet {
    pub to: Vec<NodeId>,
    pub within: f32,
}

impl ControlNode {
    pub(crate) const fn bindings(&self) -> (Option<&BindingRef>, Option<&BindingRef>) {
        match self {
            Self::Row { write, .. } | Self::Column { write, .. } => (None, write.as_ref()),
            Self::Adaptive {
                measure: Measure::Read(measure),
                ..
            } => (Some(measure), None),
            Self::Optional { hidden, .. } => (Some(hidden), None),
            Self::Slot { select, .. } => (select.as_ref(), None),
            Self::Popover { open, .. } => (Some(open), None),
            Self::Modal { open, close, .. } => (Some(open), Some(close)),
            Self::Pressable { press, .. } => (None, Some(press)),
            Self::Adaptive { .. }
            | Self::Include { .. }
            | Self::Object { .. }
            | Self::Reveal { .. }
            | Self::Scroll { .. }
            | Self::Stage { .. }
            | Self::WindowDrag { .. }
            | Self::TitleBar { .. }
            | Self::WindowControls { .. }
            | Self::Swatch { .. }
            | Self::Custom { .. }
            | Self::Shader { .. } => (None, None),
            Self::Placed { read, write, .. }
            | Self::DeckSummary { read, write, .. }
            | Self::Brand { read, write, .. }
            | Self::Spacer { read, write, .. }
            | Self::Meter { read, write, .. }
            | Self::Divider { read, write, .. }
            | Self::PresetSelector { read, write, .. }
            | Self::SettingsButton { read, write, .. }
            | Self::Text { read, write, .. }
            | Self::Glyph { read, write, .. }
            | Self::NavItem { read, write, .. }
            | Self::TabLarge { read, write, .. }
            | Self::Button { read, write, .. }
            | Self::Bpm { read, write, .. }
            | Self::Time { read, write, .. }
            | Self::Scalar { read, write, .. }
            | Self::Crossfader { read, write, .. }
            | Self::Fader { read, write, .. }
            | Self::Wave { read, write, .. }
            | Self::Vis { read, write, .. }
            | Self::Lottie { read, write, .. }
            | Self::Sprite { read, write, .. }
            | Self::Range { read, write, .. }
            | Self::Table { read, write, .. }
            | Self::Search { read, write, .. }
            | Self::Tree { read, write, .. }
            | Self::ContextBar { read, write, .. }
            | Self::Toggle { read, write, .. }
            | Self::Checkbox { read, write, .. }
            | Self::Segmented { read, write, .. }
            | Self::Select { read, write, .. }
            | Self::StatusDot { read, write, .. }
            | Self::Cell { read, write, .. }
            | Self::Readout { read, write, .. }
            | Self::Chip { read, write, .. }
            | Self::Knob { read, write, .. }
            | Self::VuStereo { read, write, .. }
            | Self::VuVertical { read, write, .. } => (read.as_ref(), write.as_ref()),
            Self::PortalMap { read, .. } => (read.as_ref(), None),
        }
    }

    pub(crate) const fn size(&self) -> Option<&SizeSpec> {
        match self {
            Self::Include { .. }
            | Self::Object { .. }
            | Self::Optional { .. }
            | Self::Placed { .. }
            | Self::Popover { .. }
            | Self::Modal { .. }
            | Self::Pressable { .. }
            | Self::Reveal { .. } => None,
            Self::Adaptive { size, .. }
            | Self::Scroll { size, .. }
            | Self::Row { size, .. }
            | Self::Column { size, .. }
            | Self::Stage { size, .. }
            | Self::Slot { size, .. }
            | Self::DeckSummary { size, .. }
            | Self::Brand { size, .. }
            | Self::Spacer { size, .. }
            | Self::Meter { size, .. }
            | Self::Divider { size, .. }
            | Self::PresetSelector { size, .. }
            | Self::SettingsButton { size, .. }
            | Self::WindowDrag { size, .. }
            | Self::TitleBar { size, .. }
            | Self::WindowControls { size, .. }
            | Self::Text { size, .. }
            | Self::Glyph { size, .. }
            | Self::NavItem { size, .. }
            | Self::TabLarge { size, .. }
            | Self::Button { size, .. }
            | Self::Bpm { size, .. }
            | Self::Time { size, .. }
            | Self::Scalar { size, .. }
            | Self::Crossfader { size, .. }
            | Self::Fader { size, .. }
            | Self::Wave { size, .. }
            | Self::Lottie { size, .. }
            | Self::Vis { size, .. }
            | Self::Sprite { size, .. }
            | Self::Shader { size, .. }
            | Self::Custom { size, .. }
            | Self::PortalMap { size, .. }
            | Self::Range { size, .. }
            | Self::Table { size, .. }
            | Self::Search { size, .. }
            | Self::Tree { size, .. }
            | Self::ContextBar { size, .. }
            | Self::Toggle { size, .. }
            | Self::Checkbox { size, .. }
            | Self::Segmented { size, .. }
            | Self::Select { size, .. }
            | Self::StatusDot { size, .. }
            | Self::Swatch { size, .. }
            | Self::Cell { size, .. }
            | Self::Readout { size, .. }
            | Self::Chip { size, .. }
            | Self::Knob { size, .. }
            | Self::VuStereo { size, .. }
            | Self::VuVertical { size, .. } => size.as_ref(),
        }
    }
}

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{
    binding::{AdaptivePolicy, BindingRef},
    motion::{Motion, Pose},
    style::{
        ButtonStyle, ChipStyle, DeckSummaryStyle, FaderStyle, GlyphStyle, IconName, PopoverAlign,
        PopoverAt, ScalarFormat, TableColumn, TextAlign, TextStyle, Tone, WaveStyle,
        WindowControlsStyle,
    },
};
use crate::{ids::NodeId, layout::FrameSides, param::Param, size::SizeSpec, skin::ColorRole};

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
        anchor: Box<Self>,
        content: Box<Self>,
    },
    /// Makes its child a click target that publishes on this node's path.
    Pressable {
        id: NodeId,
        press: BindingRef,
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
    /// Horizontal fill bar reporting one scalar.
    Meter {
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
    /// Hairline between adjacent bar cells.
    Divider {
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
    /// Bare drag surface for a window that draws its own chrome.
    WindowDrag {
        id: NodeId,
        #[serde(default)]
        size: Option<SizeSpec>,
        #[serde(default)]
        adaptive: AdaptivePolicy,
    },
    TitleBar {
        id: NodeId,
        label: String,
        #[serde(default)]
        size: Option<SizeSpec>,
        #[serde(default)]
        adaptive: AdaptivePolicy,
    },
    WindowControls {
        id: NodeId,
        #[serde(default)]
        style: WindowControlsStyle,
        #[serde(default)]
        size: Option<SizeSpec>,
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
        /// Where the glyphs sit inside the node's box.
        #[serde(default)]
        align: TextAlign,
        #[serde(default)]
        color: Option<ColorRole>,
        #[serde(default)]
        active_color: Option<ColorRole>,
        #[serde(default)]
        active: Option<BindingRef>,
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
        #[serde(default)]
        adaptive: AdaptivePolicy,
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
        #[serde(default)]
        adaptive: AdaptivePolicy,
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
        adaptive: AdaptivePolicy,
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
        adaptive: AdaptivePolicy,
        #[serde(default)]
        style: WaveStyle,
        #[serde(default)]
        badge: Option<String>,
        #[serde(default)]
        zoom: Option<BindingRef>,
    },
    Vis {
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
        #[serde(default)]
        read: Option<BindingRef>,
        #[serde(default)]
        write: Option<BindingRef>,
        seconds: f32,
        #[serde(default)]
        adaptive: AdaptivePolicy,
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
        #[serde(default)]
        adaptive: AdaptivePolicy,
    },
    /// A WGSL fragment fed by named read-only endpoint uniforms.
    Shader {
        id: NodeId,
        #[serde(default)]
        size: Option<SizeSpec>,
        source: String,
        #[serde(default)]
        uniforms: BTreeMap<String, BindingRef>,
        #[serde(default)]
        adaptive: AdaptivePolicy,
    },
    PortalMap {
        id: NodeId,
        #[serde(default)]
        size: Option<SizeSpec>,
        #[serde(default)]
        read: Option<BindingRef>,
        #[serde(default)]
        adaptive: AdaptivePolicy,
    },
    Range {
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
    Table {
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
        columns: Option<Param<Vec<TableColumn>>>,
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
        #[serde(default)]
        query: Option<BindingRef>,
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
        #[serde(default)]
        adaptive: AdaptivePolicy,
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
        /// Draw the scale left of the fader.
        #[serde(default)]
        ticks: bool,
    },
}

impl ControlNode {
    pub(crate) const fn bindings(&self) -> (Option<&BindingRef>, Option<&BindingRef>) {
        match self {
            Self::Row { write, .. } | Self::Column { write, .. } => (None, write.as_ref()),
            Self::Optional { hidden, .. } => (Some(hidden), None),
            Self::Popover { open, .. } => (Some(open), None),
            Self::Pressable { press, .. } => (None, Some(press)),
            Self::Include { .. }
            | Self::Object { .. }
            | Self::Scroll { .. }
            | Self::Stage { .. }
            | Self::Slot { .. }
            | Self::WindowDrag { .. }
            | Self::TitleBar { .. }
            | Self::WindowControls { .. }
            | Self::Swatch { .. }
            | Self::Shader { .. } => (None, None),
            Self::DeckSummary { read, write, .. }
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
            | Self::Popover { .. }
            | Self::Pressable { .. } => None,
            Self::Scroll { size, .. }
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
            | Self::PortalMap { size, .. }
            | Self::Range { size, .. }
            | Self::Table { size, .. }
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

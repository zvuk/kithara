use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{
    binding::{AdaptivePolicy, BindingRef},
    style::{
        ButtonStyle, ChipStyle, DeckSummaryStyle, FaderStyle, GlyphStyle, IconName, PopoverAlign,
        PopoverAt, ScalarFormat, TextAlign, TextStyle, Tone, TrackColumn, WaveStyle,
        WindowControlsStyle,
    },
};
use crate::{ids::NodeId, layout::FrameSides, param::Param, size::SizeSpec, skin::ColorRole};

const fn default_framed() -> bool {
    true
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
        /// Fill behind the children; absent means transparent.
        #[serde(default)]
        background: Option<ColorRole>,
        #[serde(default)]
        background_alpha: Option<f32>,
        #[serde(default)]
        write: Option<BindingRef>,
        children: Vec<Self>,
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
        columns: Option<Param<Vec<TrackColumn>>>,
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
        tone: Tone,
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
    pub(crate) fn bindings(&self) -> (Option<&BindingRef>, Option<&BindingRef>) {
        match self {
            Self::Row { write, .. } | Self::Column { write, .. } => (None, write.as_ref()),
            Self::Optional { hidden, .. } => (Some(hidden), None),
            Self::Popover { open, .. } => (Some(open), None),
            Self::Pressable { press, .. } => (None, Some(press)),
            Self::Include { .. }
            | Self::Slot { .. }
            | Self::WindowDrag { .. }
            | Self::TitleBar { .. }
            | Self::WindowControls { .. }
            | Self::Swatch { .. } => (None, None),
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
            | Self::TrackList { read, write, .. }
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
        }
    }

    pub(crate) fn size(&self) -> Option<&SizeSpec> {
        match self {
            Self::Include { .. }
            | Self::Optional { .. }
            | Self::Popover { .. }
            | Self::Pressable { .. } => None,
            Self::Row { size, .. }
            | Self::Column { size, .. }
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
            | Self::Vis { size, .. }
            | Self::TrackList { size, .. }
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

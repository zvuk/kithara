use std::collections::BTreeMap;

use crate::{
    error::UiDocError,
    ids::{InternId, SourceUri},
    layout::FrameSides,
    module::{
        BindingRef, ButtonStyle, ChipStyle, ChromeStyle, ControlNode, DeckSummaryStyle, FaderStyle,
        GlyphStyle, IconName, MeasureAxis, PopoverAlign, PopoverAt, ScalarFormat, TextAlign,
        TextStyle, Tone, TrackColumn, WaveStyle, WindowControlsStyle,
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
        measure: Option<MeasureAxis>,
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
    /// Draws one branch: the last step whose threshold the measure reaches,
    /// and `base` below the first of them.
    Adaptive {
        measure: MeasureSpec,
        size: Option<SizeSpec>,
        base: Box<Self>,
        steps: Vec<(f32, Self)>,
    },
    /// Laid out once the enclosing container measures `from` on the axis it
    /// declares.
    Reveal {
        from: f32,
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
    PortalMap,
    Range,
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
    /// The axis of the declared box this measure reads, if it reads its own.
    pub(crate) const fn axis(&self) -> Option<MeasureAxis> {
        match self {
            Self::Width => Some(MeasureAxis::Width),
            Self::Height => Some(MeasureAxis::Height),
            Self::Read(_) => None,
        }
    }

    /// The binding a read measure resolves, if the host answers this one.
    pub(crate) const fn binding(&self) -> Option<&Binding> {
        match self {
            Self::Read(binding) => Some(binding),
            Self::Width | Self::Height => None,
        }
    }
}

/// The branch a measure selects: the last step the value reaches, and `base`
/// below every threshold or when nothing was read.
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

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    /// Branches that differ only by a number the selector never reads.
    fn form(gap: f32) -> ExpandedNode {
        ExpandedNode::Row {
            gap: Some(gap),
            id: None,
            size: None,
            measure: None,
            pad: None,
            pad_x: None,
            pad_y: None,
            frame: None,
            background: None,
            background_alpha: None,
            active: None,
            active_background: None,
            frame_color: None,
            active_frame_color: None,
            surface: None,
            children: Vec::new(),
        }
    }

    #[kithara::test]
    fn a_measure_takes_the_last_step_it_reaches() {
        let base = form(0.0);
        let steps = vec![(4.0, form(4.0)), (8.0, form(8.0))];
        let branch = |value| adaptive_branch(&base, &steps, value);

        assert_eq!(branch(None), &base, "nothing read");
        assert_eq!(branch(Some(3.9)), &base, "below the first step");
        assert_eq!(branch(Some(4.0)), &steps[0].1, "exactly on a threshold");
        assert_eq!(branch(Some(7.9)), &steps[0].1);
        assert_eq!(branch(Some(8.0)), &steps[1].1);
        assert_eq!(branch(Some(f32::MAX)), &steps[1].1, "above every step");
        assert_eq!(branch(Some(f32::NAN)), &base, "ordered against nothing");
    }
}

use masonry::{
    core::{BoxConstraints, LayoutCtx, WidgetPod},
    kurbo::{Point, Size as MasonrySize},
};
use num_traits::cast::AsPrimitive;

use super::{
    MasonryHost, MasonryNode, Painted,
    controls::Retained,
    flex::{box_constraints, normalized},
    leaf::{DragProgram, Leaf},
    node::Node,
};
use crate::{
    atoms::{button::declared_width, deck::summary::Summary, tab::TabLarge},
    expand::{Binding, ControlSpec},
    module::{TextAlign, TextStyle},
    mount,
    render::{
        InputOwner, ReadValue, Skin,
        controls::{Draws, Reading},
        document::read::{read_scope, resolve},
    },
    size::{Dim, SizeSpec, control_size},
    skin::{ColorRole, TextRoleSkin},
    solve,
    widgets::window::{ControlsProgram, TitleProgram},
};

/// How one built-in control becomes a leaf of the retained tree.
///
/// The default is an empty box of the right size: this host paints a control
/// only once its painter is neutral, and until then the control still holds its
/// place. Which controls are still waiting is the census in `tests`, not a
/// silent arm in a match.
pub(super) trait NodeControl {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        host.empty(cx.declared)
    }

    /// Anything the host must still attach once the leaf exists: a window layer
    /// for the controls that move the window, a settings action for the one
    /// that opens it.
    fn wire<A>(&self, host: &MasonryHost<'_, A>, output: &mut MasonryNode<A>)
    where
        A: std::fmt::Debug + Send + 'static,
    {
        let _ = (host, output);
    }
}

/// What a control is handed when it mounts: the box it was given, the endpoint
/// behind it, and who owns the pointer over it.
pub(super) struct Cx<'a> {
    pub(super) declared: solve::Size<solve::Length>,
    pub(super) owner: InputOwner,
    pub(super) path: &'a str,
    pub(super) read: Option<&'a Binding>,
}

impl NodeControl for mount::Summary {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        painted(self, host, cx)
    }
}
impl NodeControl for mount::Brand {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        painted(self, host, cx)
    }
}
impl NodeControl for mount::Spacer {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        painted(self, host, cx)
    }
}
impl NodeControl for mount::Divider {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        painted(self, host, cx)
    }
}
impl NodeControl for mount::Preset {}
impl NodeControl for mount::Settings {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        painted(self, host, cx)
    }
}

impl NodeControl for mount::Drag {
    fn wire<A>(&self, host: &MasonryHost<'_, A>, output: &mut MasonryNode<A>)
    where
        A: std::fmt::Debug + Send + 'static,
    {
        host.add_window_layer(output, DragProgram);
    }
}

impl NodeControl for mount::TitleBar {
    fn wire<A>(&self, host: &MasonryHost<'_, A>, output: &mut MasonryNode<A>)
    where
        A: std::fmt::Debug + Send + 'static,
    {
        host.add_window_layer(
            output,
            TitleProgram::new(host.ui.resolve(self.label), host.skin),
        );
    }
}

impl NodeControl for mount::Controls {
    fn wire<A>(&self, host: &MasonryHost<'_, A>, output: &mut MasonryNode<A>)
    where
        A: std::fmt::Debug + Send + 'static,
    {
        host.add_window_layer(output, ControlsProgram::new(self.style, host.skin));
    }
}

impl NodeControl for mount::Glyph<'_> {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        painted(self, host, cx)
    }
}
impl NodeControl for mount::Bpm {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        painted(self, host, cx)
    }
}
impl NodeControl for mount::Time {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        painted(self, host, cx)
    }
}
impl NodeControl for mount::Telemetry {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        painted(self, host, cx)
    }
}
impl NodeControl for mount::Wave<'_> {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        painted(self, host, cx)
    }
}
impl NodeControl for mount::Vis {}
impl NodeControl for mount::TrackList<'_> {}
impl NodeControl for mount::Tree<'_> {}
impl NodeControl for mount::ContextBar<'_> {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        painted(self, host, cx)
    }
}
impl NodeControl for mount::Segmented<'_> {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        painted(self, host, cx)
    }
}
impl NodeControl for mount::Select {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        painted(self, host, cx)
    }
}
impl NodeControl for mount::Readout {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        painted(self, host, cx)
    }
}

impl NodeControl for mount::Text<'_> {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        let content = cx
            .read
            .and_then(|binding| resolve(host.reads, binding, host.ui))
            .and_then(|value| match value {
                ReadValue::Text(value) => Some(value.to_owned()),
                _ => None,
            })
            .or_else(|| self.label.map(|label| host.ui.resolve(label).to_owned()))
            .unwrap_or_default();
        host.text_leaf(
            content,
            self.style,
            self.color,
            self.active_color,
            host.reads_true(self.active),
            cx.declared,
        )
    }
}

impl NodeControl for mount::Knob {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        painted(self, host, cx)
    }
}

impl NodeControl for mount::Chip {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        painted(self, host, cx)
    }
}

impl NodeControl for mount::Tab {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        painted(self, host, cx)
    }
}

/// An unbound meter is an empty track rather than an empty box: that is what
/// the other host has always drawn for it.
impl NodeControl for mount::Meter {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        painted(self, host, cx)
    }
}

impl NodeControl for mount::Cell {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        painted(self, host, cx)
    }
}

impl NodeControl for mount::Swatch {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        painted(self, host, cx)
    }
}

impl NodeControl for mount::StatusDot {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        painted(self, host, cx)
    }
}

impl NodeControl for mount::Toggle {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        painted(self, host, cx)
    }
}

impl NodeControl for mount::Checkbox {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        painted(self, host, cx)
    }
}

impl NodeControl for mount::VuVertical {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        painted(self, host, cx)
    }
}

impl NodeControl for mount::VuStereo {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        painted(self, host, cx)
    }
}

impl NodeControl for mount::Crossfader {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        painted(self, host, cx)
    }
}

impl NodeControl for mount::Fader {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        painted(self, host, cx)
    }
}

impl NodeControl for mount::NavItem {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        painted(self, host, cx)
    }
}

impl NodeControl for mount::Button {
    fn leaf<A>(&self, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
    where
        A: std::fmt::Debug + Send + 'static,
    {
        painted(self, host, cx)
    }
}

pub(crate) enum NodeLayout {
    Leaf(Leaf),
    Flex(super::flex::Flex),
    Stack,
}

impl NodeLayout {
    pub(crate) fn layout(
        &mut self,
        ctx: &mut LayoutCtx<'_>,
        children: &mut [WidgetPod<Node>],
        limits: solve::Limits,
        declared: solve::Size<solve::Length>,
    ) -> solve::Size {
        match self {
            Self::Leaf(leaf) => {
                let intrinsic = leaf.measure(limits);
                limits.resolve(declared.width, declared.height, intrinsic)
            }
            Self::Flex(flex) => {
                let intrinsic = flex.layout(ctx, children, limits);
                limits.resolve(declared.width, declared.height, intrinsic)
            }
            Self::Stack => stack(ctx, children, limits, declared),
        }
    }

    pub(crate) const fn leaf(&mut self) -> Option<&mut Leaf> {
        match self {
            Self::Leaf(leaf) => Some(leaf),
            Self::Flex(_) | Self::Stack => None,
        }
    }

    pub(crate) fn accepts_input(&self) -> bool {
        matches!(self, Self::Leaf(leaf) if leaf.accepts_input())
    }

    pub(crate) fn accepts_text_input(&self) -> bool {
        matches!(self, Self::Leaf(leaf) if leaf.accepts_text_input())
    }
}

fn stack(
    ctx: &mut LayoutCtx<'_>,
    children: &mut [WidgetPod<Node>],
    limits: solve::Limits,
    declared: solve::Size<solve::Length>,
) -> solve::Size {
    let inner = normalized(limits.width(declared.width).height(declared.height).loose());
    let intrinsic = children.first_mut().map_or(solve::Size::ZERO, |first| {
        Node::set_child_limits(ctx, first, inner);
        let size = ctx.run_layout(first, &box_constraints(inner));
        solve::Size::new(size.width.as_(), size.height.as_())
    });
    let size = limits.resolve(declared.width, declared.height, intrinsic);
    let exact = solve::Limits::new(size, size);
    for child in children {
        Node::set_child_limits(ctx, child, exact);
        ctx.run_layout(
            child,
            &BoxConstraints::tight(MasonrySize::new(
                f64::from(size.width),
                f64::from(size.height),
            )),
        );
        ctx.place_child(child, Point::ORIGIN);
    }
    size
}

pub(crate) const fn main_length(dim: Dim) -> solve::Length {
    match dim {
        Dim::Fixed(value) => solve::Length::Fixed(value),
        Dim::Range { .. } | Dim::Fill | Dim::Shrink => solve::Length::Fill,
    }
}

pub(crate) const fn length(dim: Dim) -> solve::Length {
    match dim {
        Dim::Fixed(value) => solve::Length::Fixed(value),
        Dim::Shrink => solve::Length::Shrink,
        Dim::Range { .. } | Dim::Fill => solve::Length::Fill,
    }
}

pub(crate) const fn declared(size: SizeSpec) -> solve::Size<solve::Length> {
    solve::Size::new(length(size.w), length(size.h))
}

pub(crate) fn control_declared(
    spec: &ControlSpec,
    size: Option<SizeSpec>,
    skin: &Skin,
) -> solve::Size<solve::Length> {
    let intrinsic = match spec {
        ControlSpec::DeckSummary { .. } => Summary::declared_length(skin.deck),
        ControlSpec::Button { style, .. } => {
            solve::Size::new(declared_width(*style, skin), solve::Length::Fill)
        }
        ControlSpec::TabLarge { .. } => TabLarge::declared_length(skin.tab_large.height),
        ControlSpec::Text { .. } => solve::Size::new(solve::Length::Shrink, solve::Length::Fill),
        ControlSpec::Spacer | ControlSpec::WindowDrag | ControlSpec::TitleBar { .. } => {
            solve::Size::new(solve::Length::Fill, solve::Length::Fill)
        }
        _ => declared(control_size(spec, skin.document())),
    };
    size.map_or(intrinsic, |size| {
        solve::Size::new(
            control_length(size.w, intrinsic.width),
            control_length(size.h, intrinsic.height),
        )
    })
}

pub(crate) const fn control_length(dim: Dim, intrinsic: solve::Length) -> solve::Length {
    match dim {
        Dim::Fixed(value) => solve::Length::Fixed(value),
        Dim::Shrink => solve::Length::Shrink,
        Dim::Range { .. } => match intrinsic {
            solve::Length::FillPortion(portion) => solve::Length::FillPortion(portion),
            solve::Length::Fill | solve::Length::Shrink | solve::Length::Fixed(_) => {
                solve::Length::Fill
            }
        },
        Dim::Fill => solve::Length::Fill,
    }
}

pub(crate) const fn alignment(value: TextAlign) -> solve::Alignment {
    match value {
        TextAlign::Start => solve::Alignment::Start,
        TextAlign::Center => solve::Alignment::Center,
        TextAlign::End => solve::Alignment::End,
    }
}

pub(crate) fn text_role(
    style: TextStyle,
    color: Option<ColorRole>,
    active_color: Option<ColorRole>,
    active: bool,
    skin: &Skin,
) -> TextRoleSkin {
    let (role, skin_active) = match style {
        TextStyle::Body => (skin.text.body, None),
        TextStyle::Brand => (skin.text.brand, None),
        TextStyle::BrandSmall => (skin.text.brand_small, None),
        TextStyle::DeckLetter => (skin.text.deck_letter, Some(skin.text.deck_letter_active)),
        TextStyle::TrackTitle => (skin.text.track_title, None),
        TextStyle::Telemetry => (skin.text.telemetry, None),
        TextStyle::MicroLabel => (skin.text.micro_label, None),
        TextStyle::Section => (skin.text.section, None),
        TextStyle::Mono => (skin.text.mono, None),
        TextStyle::Caption => (skin.text.caption, None),
        TextStyle::VisFooter | TextStyle::VisMeta => (skin.vis.meta, None),
        TextStyle::VisTitle => (skin.vis.title, None),
    };
    TextRoleSkin {
        color: active
            .then_some(active_color.or(skin_active))
            .flatten()
            .or(color)
            .unwrap_or(role.color),
        ..role
    }
}

/// Who answers the pointer over this control.
///
/// The document says whether the leaf may own it at all; this host narrows that
/// to the controls it actually paints. One it still mounts as an empty box is
/// driven by the engine plan, and a leaf gesture beside that plan would be two
/// recognizers on one pointer.
pub(crate) fn pointer_owner(owner: InputOwner, spec: &ControlSpec) -> InputOwner {
    if owner == InputOwner::Leaf && leaf_paints(spec) {
        InputOwner::Leaf
    } else {
        InputOwner::Engine
    }
}

/// Whether this control reaches Vello as a painted leaf that can own the
/// pointer itself, rather than as an empty box the engine drives.
fn leaf_paints(spec: &ControlSpec) -> bool {
    match spec {
        ControlSpec::Button { .. }
        | ControlSpec::Chip { .. }
        | ControlSpec::Knob { .. }
        | ControlSpec::NavItem { .. }
        | ControlSpec::SettingsButton
        | ControlSpec::TabLarge { .. } => true,
        _ => false,
    }
}

pub(crate) const fn activates(spec: &ControlSpec) -> bool {
    matches!(
        spec,
        ControlSpec::NavItem { .. }
            | ControlSpec::TabLarge { .. }
            | ControlSpec::Button { .. }
            | ControlSpec::Toggle
            | ControlSpec::Checkbox
            | ControlSpec::Chip { .. }
    )
}

/// Mounts a control that draws itself, adding nothing to the picture.
fn painted<Control, A>(control: &Control, host: &MasonryHost<'_, A>, cx: &Cx<'_>) -> MasonryNode<A>
where
    Control: Draws,
    Control::Painter: Retained + 'static,
    A: std::fmt::Debug + Send + 'static,
{
    let value = cx
        .read
        .and_then(|binding| resolve(host.reads, binding, host.ui));
    let Some(data) = control.data(Reading {
        reads: host.reads,
        scope: read_scope(cx.read, host.ui),
        ui: host.ui,
        value: value.as_ref(),
    }) else {
        return host.empty(cx.declared);
    };
    let grip = control.grip(host.skin, &data);
    let leaf = Painted::new(control.painter(host.skin), data, host.skin);
    let leaf = host.owned(leaf, cx.owner, cx.path, |leaf, path, map_event| {
        leaf.interactive(grip, path, map_event)
    });
    host.control_leaf(leaf, cx.declared)
}

/// Which controls this host lets answer the pointer themselves.
#[cfg(test)]
mod owns {
    use kithara_test_utils::kithara;

    use super::{ControlSpec, InputOwner, pointer_owner};

    /// A control this host still mounts as an empty box has an engine plan
    /// behind it. Handing its leaf a gesture as well would put two recognizers
    /// on one pointer, and the document cannot see the difference to say so.
    #[kithara::test]
    fn a_control_this_host_does_not_paint_is_left_to_the_engine() {
        assert_eq!(
            pointer_owner(InputOwner::Leaf, &ControlSpec::VuVertical { ticks: false }),
            InputOwner::Engine
        );
    }

    #[kithara::test]
    fn a_control_this_host_paints_keeps_the_leaf_the_document_gave_it() {
        assert_eq!(
            pointer_owner(InputOwner::Leaf, &ControlSpec::Knob { label: None }),
            InputOwner::Leaf
        );
    }

    /// And a document that kept the pointer for the engine keeps it, whatever
    /// this host can paint.
    #[kithara::test]
    fn an_engine_owned_control_stays_engine_owned() {
        assert_eq!(
            pointer_owner(InputOwner::Engine, &ControlSpec::Knob { label: None }),
            InputOwner::Engine
        );
    }
}
